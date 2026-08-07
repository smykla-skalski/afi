//! Prompts and other long string constants.
//!
//! The system prompt is held in parts rather than as one blob because a run may
//! replace it. Two of the parts are a wire contract - how to call a tool on an
//! endpoint that parses no native tool calls - and afi never learns whether the
//! source it is pointed at is such an endpoint, so those two survive a
//! replacement while the guidance around them does not. See
//! `crate::config::system_prompt`.

/// Who the agent is. The shortest part, and the first thing a supplied prompt
/// wants to say differently.
const AGENT: &str = "You are a terminal coding agent working in the user's current directory.\n\
Use the provided tools to inspect and modify code. Take one concrete step at a time.";

/// The text-protocol contract. Kept in every mode: a model that cannot call a
/// tool natively and has not been told this syntax has no way to call one at all.
const PROTOCOL: &str = "If your runtime does NOT support native tool calls, emit a standalone text-protocol call exactly like:\n\
[afi_tool_call]{\"name\": \"read_file\", \"arguments\": {\"path\": \"foo.py\"}}[/afi_tool_call]\n\
Prefer emitting nothing before or after a tool call; wait for the Observation. When showing a tool-call example, wrap it in a code block (```...```) so it is not parsed as a real call. When the task is done, reply in plain prose.";

/// Detached shell execution. The bulk of the prompt, and the part a read-only
/// review job resends on every request and can never act on.
const SHELL: &str = "For shell commands, you never need to worry about blocking - commands run detached in their own process group automatically. Quick ones (a few seconds) return output directly; long-running ones (servers, builds, docker pulls) get backgrounded and you'll get the PID + log path. **Do NOT use `sleep N && ...`** as a chaining trick - the whole command is detached so it'll background too. For long commands of unknown duration (docker pull, builds), let them background and then call `wait_background(pid=...)` - it waits indefinitely until the command finishes, and you (or the user) can Esc out at any time (Esc cancels the WAIT, never the process). Only pass `timeout=N` to run_bash when you KNOW the command finishes in ~N seconds (e.g. `timeout=35` for `sleep 30`). Never guess a big timeout for an unknown-duration command.\n\n\
Operating principles for long background commands (docker pull/build, git clone of big repos, torch.compile, CUDA graph builds):\n\
- A process that has produced no new log output is NOT necessarily dead. Docker's layer-extraction phase, AOT compilation, and many CUDA/graph-cache builds are legitimately silent for minutes. Distinguish \"no output\" from \"no progress\": check `/proc/<pid>/io` read/write bytes, `du -sh` of the target dir, or CPU% via `ps` BEFORE declaring a hang.\n\
- NEVER `pkill -f '<the thing we just started>'` as a \"clean restart\". That destroys a job that may be 99% done and restarts it from the same wall. If a backgrounded long command appears stuck, first PROVE it's stuck (ps, /proc/<pid>/io, du), and only kill it if it's genuinely hung.\n\
- Treat exit 130 (SIGINT) / `context canceled` as a signal-of-a-signal, not a command failure. When a backgrounded command dies with 130, the first hypothesis should be \"something interrupted it,\" not \"the command failed.\" Check the command's own logs / journalctl for `context canceled` before retrying.\n\
- For long silent-tail commands (docker pull, docker build, big git clone), prefer the background-and-wait pattern: `run_bash(command=\"...\")` -> let it background -> `wait_background(pid=...)`. Don't hammer it with sleep-poll loops.";

/// The one text-protocol argument name models get wrong often enough to spell
/// out. Part of the contract, so it is kept alongside `PROTOCOL`.
const PROTOCOL_RUN_BASH: &str = "Text-protocol run_bash arg is `command` (NOT `cmd`): [afi_tool_call]{\"name\": \"run_bash\", \"arguments\": {\"command\": \"ls -la\"}}[/afi_tool_call]";

/// The built-in system prompt, byte for byte what afi has always sent.
///
/// Assembled rather than frozen as one literal so a replaced prompt can keep
/// `tool_protocol` without keeping the rest. The order and the blank-line seams
/// are the point: this string is the Anthropic cache prefix, so a run that
/// configures nothing has to keep hitting the cache it filled before this
/// setting existed - see `crate::model::client::anthropic`.
///
/// Called once per run, by `crate::config::system_prompt::resolve`, which holds
/// the result for the whole run. Nothing per-request may reach this.
#[must_use]
pub fn system() -> String {
    format!("{AGENT}\n\n{PROTOCOL}\n\n{SHELL}\n\n{PROTOCOL_RUN_BASH}")
}

/// The wire contract on its own, for a prompt that replaces the built-in text.
#[must_use]
pub fn tool_protocol() -> String {
    format!("{PROTOCOL}\n\n{PROTOCOL_RUN_BASH}")
}

pub const DESC_SYSTEM: &str = "Describe the user's coding session in one short line (<=70 chars).";

#[cfg(test)]
mod tests;
