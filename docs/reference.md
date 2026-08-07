# Reference

Flags, environment variables, subcommands, and slash commands for `afi`. See the [main README](../README.md) for setup and concepts.

## Flags

| flag                                        | what it does                                                |
| ------------------------------------------- | ----------------------------------------------------------- |
| `--config <path>`                           | read settings from this file instead of the default ([details](#config-file)) |
| `--yolo`                                    | start in never-prompt mode (auto-approve everything)        |
| `--approval <all\|low\|medium\|high\|yolo>` | start with a non-default approval mode                      |
| `--source <name>`                           | start on a specific source                                  |
| `--resume [target]`                         | resume a saved session - bare = most recent                 |
| `--session <id>`                            | start a fresh run attached to a specific session id         |
| `--prompt-file <path>` / `-f`               | non-interactive single-shot mode (reads from file or stdin) |
| `--summary json`                            | print a machine-readable run summary on stdout ([details](#run-summary)) |
| `--summary-file <path>`                     | also write that summary to a path ([details](#writing-the-summary-to-a-file)) |
| `--effort <low\|medium\|high\|xhigh\|max>`  | how hard the model is asked to think ([details](#reasoning-effort))  |
| `--system-prompt-file <path>`               | send these standing instructions to the model ([details](#system-prompt)) |
| `--system-prompt-mode <replace\|append>`    | against the built-in prompt, default `replace` ([details](#system-prompt)) |
| `--read-only`                               | deny every tool that can change anything ([details](#tool-policy)) |
| `--allowed-tools <a,b>`                     | only these tools may be called ([details](#tool-policy))    |
| `--disallowed-tools <a,b>`                  | these tools may not be called ([details](#tool-policy))     |
| `--version` / `-V`                          | print the version and build metadata ([details](#version-and-build-metadata)) |
| `--help` / `-h`                             | print usage and exit                                        |

`--help` and `--version` are answered before anything else, so neither depends on an env file loading or a source resolving, and both work as the last word of a command you were already typing. `--help` wins when both are given.

**A long flag takes its value either way**, so `--source zai` and `--source=zai` are the same flag, and `--source=` goes without a value exactly as the spaced form does. A flag that is a statement by itself refuses a value written into one: `--read-only=false` reads as "off" to whoever typed it, and taking it as a bare `--read-only` would turn the posture on instead.

**An argument afi does not have refuses the run**, naming it. Every one of them used to be ignored, so `--red-only` left a run with writes enabled while the command line said otherwise, and a flag that went without its value was dropped just as quietly. afi reads its prompt from `-f`, never from a bare word, so a stray one is refused too.

## Version and build metadata

`afi --version` identifies the exact binary, one `label: value` per line so it can be read with `grep` rather than a JSON parser:

```
afi 0.2.0
  commit:      eab85680abce54c56e1ce07f0c51208288ab7f02
  commit-date: 2026-08-06T17:25:27+02:00
  target:      aarch64-apple-darwin
  profile:     release
  rustc:       1.97.1
  executable:  /home/you/.cargo/bin/afi
  sha256:      1cec5f3a9eb9663b5d9cce1d12d1b8c792cdc6f38d8d3504fd6203137a294be7
```

`sha256` is the digest of the executable itself, computed as it runs, so it matches `sha256sum $(command -v afi)` and can be recorded from a CI log to prove which binary produced a result. It is not the digest of the release tarball, which covers the archive rather than the file inside it.

`commit` carries a `(dirty)` marker when tracked files had been modified relative to that commit, since otherwise the sha would describe code that was never compiled. `commit-date` is the commit's own date rather than a build timestamp: cargo caches build-script output, so a "built at" stamp would record when the script last ran and drift away from the binary beside it.

Every field except the version and the profile is best-effort. A build with no git repository, or with no `git` binary at all - a release tarball, `cargo install` from a registry, a container holding only the sources - reports `unknown` instead of failing to compile. `AFI_BUILD_COMMIT` and `AFI_BUILD_COMMIT_DATE` pin those two fields at build time for exactly that case, and `GITHUB_SHA` is picked up automatically, so CI on the runner itself needs no wiring. A build inside a container has to pass the variable in, since neither the runner's environment nor its git repository is visible there by default.

## Environment variables

| env var                                      | what it does                                                         |
| -------------------------------------------- | -------------------------------------------------------------------- |
| `AFI_APPROVAL`                               | persistent default approval mode: `all`/`low`/`medium`/`high`/`yolo` |
| `AFI_BASE_URL` / `AFI_MODEL` / `AFI_API_KEY` | legacy single-source config                                          |
| `AFI_SOURCES` / `AFI_SOURCE_*`               | named multi-source endpoints                                         |
| `AFI_ACTIVE`                                 | name of the source to start on                                       |
| `AFI_TOGETHER_API_KEY`                       | auto-registers a built-in `together` source                          |
| `AFI_OPENROUTER_API_KEY`                     | auto-registers a built-in `openrouter` source                        |
| `ANTHROPIC_API_KEY`                          | auto-registers a built-in `anthropic` source ([details](#anthropic)) |
| `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` | auto-registers a built-in `bedrock` source ([details](#bedrock))    |
| `AWS_REGION` / `AWS_SESSION_TOKEN`           | the Region that source signs for, and an STS token ([details](#bedrock)) |
| `AWS_ROLE_ARN`                               | registers the same source with no key at all ([details](#bedrock-without-a-key)) |
| `AFI_BACKEND`                                | set to `vllm` to disable llama.cpp-only recovery knobs               |
| `AFI_HOME` / `AFI_SESSIONS_DIR`              | where session JSON files are stored                                  |
| `AFI_AUTOCOMPRESS_PERCENT`                   | auto-compress threshold (default 85, 0=off)                          |
| `AFI_MAX_TOKENS`                             | token cap for normal streaming requests (default 16000)              |
| `AFI_READ_FILE_LINES`                        | default lines returned by `read_file` (default 400)                  |
| `AFI_TOOL_RESULT_CHARS`                      | per-tool-result char cap (default 20000)                             |
| `AFI_SUMMARY`                                | set to `json` for a run summary on stdout ([details](#run-summary))  |
| `AFI_SUMMARY_FILE`                           | path to also write the run summary to ([details](#writing-the-summary-to-a-file)) |
| `AFI_EFFORT`                                 | reasoning effort for every source ([details](#reasoning-effort))     |
| `AFI_SYSTEM_PROMPT_FILE`                     | standing instructions to send instead of afi's ([details](#system-prompt)) |
| `AFI_SYSTEM_PROMPT_MODE`                     | `replace` (default) or `append` ([details](#system-prompt))           |
| `AFI_ALLOWED_TOOLS` / `AFI_DISALLOWED_TOOLS` | restrict which tools a run may call ([details](#tool-policy))         |
| `AFI_READ_ONLY`                              | deny every tool that can change anything ([details](#tool-policy))    |
| `AFI_PRICES`                                 | per-model token rates, so the summary reports cost ([details](#cost)) |
| `AFI_CONFIG`                                 | read settings from this file instead of the defaults ([details](#config-file)) |

Most of these can be written in a [config file](#config-file) instead, where a variable beats the file. No credential can: that section says where they go and which other names are absent.

## Config file

Everything above is a flat string, so anything with structure has to be flattened into one: a source becomes a set of variables whose names encode its name, and the price table and the Anthropic extra body become JSON squeezed onto one line of shell. A misspelled variable is skipped in silence, so the run starts with the setting you thought you had set simply absent.

A config file is a second way in for the same settings. afi reads two, lowest precedence first:

| file                                                             | written by                                    |
| ---------------------------------------------------------------- | --------------------------------------------- |
| `$AFI_HOME/config.json`, `~/.afi/config.json` unless you moved it | you, so it sets anything                     |
| the nearest `.afi/config.json` at or above the working directory  | the repository, so it sets less - see below   |

The project walk stops at the directory holding `.git`, so a file above the repository belongs to whatever is up there rather than to this project. Outside a repository only the working directory is checked.

```json
{
  "active": "zai",
  "effort": "high",
  "read_only": true,
  "sources": {
    "zai": {
      "base_url": "https://api.z.ai/api/paas/v4",
      "model": "glm-4.6"
    },
    "local": { "base_url": "http://localhost:8080/v1" }
  },
  "source_order": ["zai", "local"],
  "prices": {
    "glm-4.6": { "input": 0.6, "output": 2.2 }
  },
  "anthropic": {
    "model": "claude-opus-5",
    "extra_body": { "thinking": { "type": "adaptive", "display": "summarized" } }
  }
}
```

**A flag beats a variable, a variable beats the file, and the file beats the built-in default.** An entry in the env file counts as the variable, since nothing downstream can tell the two apart, so a half-migrated setup keeps working rather than changing the moment a config file appears. A variable exported with no value still counts as set, because for several of these a blank is how you turn the setting off - `AFI_SUMMARY_FILE=` names no file, and filling it from the file would write one you suppressed. A run with no config file behaves exactly as it did before there was one.

**No config file holds a credential.** A config file is a thing people commit, paste into an issue, and copy between machines, and the one kind of value that must not travel that way is the kind that authenticates. `api_key`, `oauth_token`, `together_api_key`, `openrouter_api_key`, and `anthropic.federation.identity_token_file` are refused by name, with the variable to set instead:

```
  ✗ config.json: sources.zai.api_key a credential does not go in a config file - set AFI_SOURCE_<NAME>_API_KEY, in the environment or in the env file
```

Credentials stay in the environment or the [env file](#environment-variables), which is where the tooling around secrets already looks. A source with no `api_key` anywhere sends none, which is what a local llama.cpp wants.

**A project file sets what a repository has a say in, and no more.** It is written by whoever wrote the repository rather than by whoever is running afi, so `cd`-ing into a clone must not reconfigure the run. Given the whole keyspace it could: one key redirecting a source's `base_url` is enough for the clone to receive whatever credential your environment holds, and `approval` in the same file switches off the gate that would have asked.

So a project file may say **what to work with** - `active`, `source_order`, a source's `model`, `effort`, `backend`, `max_tokens` and the other sizing and tuning knobs, `summary`, `prices`, a source's `extra_body`, `app_name`, and `app_url`. It may not say where requests go, whose instructions the model follows, or whether you are asked:

| refused in a project file                                        | why                                              |
| ---------------------------------------------------------------- | ------------------------------------------------ |
| `sources.*.base_url`, `sources.*.protocol`, `anthropic.base_url`  | where requests go, and what credential goes with them |
| `anthropic.federation.*`                                          | whose credential is exchanged                   |
| `approval`                                                        | whether you are asked before a tool runs        |
| `system_prompt_file`, `system_prompt_mode`                         | whose instructions the model follows            |
| `summary_file`, `home`, `sessions_dir`                             | where afi writes                                |

Reaching for one of those from a project file refuses the run and says so, naming the key.

**The tool policy is the exception, and it may only tighten.** A repository saying "this project is read-only", or naming fewer tools than you allowed, is a thing it should be able to say - so `read_only`, `allowed_tools`, and `disallowed_tools` are permitted from a project file, and the three combine rather than replace when both files set them. Deny lists add up, allow lists keep only what both agree on, and `read_only` stays on once either asks for it. So a project file can take a tool away and cannot hand one back:

| your file                          | the project's                | the run gets              |
| ---------------------------------- | ---------------------------- | ------------------------- |
| `"allowed_tools": ["read_file"]`    | `["read_file", "run_bash"]`  | `read_file`               |
| `"disallowed_tools": ["run_bash"]`  | `[]`                         | `run_bash` still denied   |
| `"read_only": true`                 | `false`                      | read-only                 |

Two allow lists with nothing in common are a conflict between the files rather than a value either got wrong, so the run exits 2 saying which tools each one permits. It cannot be answered with an empty list: that reads as "every tool" by the time it reaches the policy, so the run would end up with every tool precisely because two files agreed on none.

`prices` and a source's `extra_body` combine key by key too, the later file winning per key. A project file pricing one model leaves your rates for the others standing, where replacing would have dropped them and taken `cost_usd` quiet with them.

`--config <path>` reads a file with your full trust, whatever directory it sits in, because naming a path is the act of trust - so `afi --config ./.afi/config.json` opts into a repository's file whole.

**Every key is its variable, minus the `AFI_` prefix and lowercased.** `AFI_MAX_TOKENS` is `max_tokens`, `AFI_READ_ONLY` is `read_only`, and so on through the table above and the tuning variables that are not in it. A test reads the source for `AFI_*` names and fails when one has neither a key nor a stated reason, so this stays true as settings are added. Four groups have structure instead:

| key             | what it replaces                                                              |
| --------------- | ----------------------------------------------------------------------------- |
| `sources`       | the `AFI_SOURCE_<NAME>_*` variables, one object per source, keyed by its name  |
| `source_order`  | `AFI_SOURCES`                                                                 |
| `prices`        | `AFI_PRICES`, as an object rather than as JSON inside a string                 |
| `anthropic`     | `AFI_ANTHROPIC_*`, with `anthropic.federation` holding the `ANTHROPIC_*` federation ids |

A source takes `base_url`, `model`, `protocol`, `app_name`, `app_url`, and `extra_body` - no `api_key`, which is the one above. `extra_body` is a JSON object here, not a string of one. Object key order is not preserved, so name the order you want in `source_order` rather than relying on the order you wrote the sources in. A source's name has to be lowercase, with digits, `-`, and `_` allowed: the name becomes part of a variable name, which is uppercased on the way in and lowercased on the way back out, so a source written `Zai` would register as `zai` and `"active": "Zai"` would then match nothing.

`home` and `sessions_dir` move what afi writes, not where this file is read from - the file has to be found before it can say anything. Point `AFI_HOME` at the directory to move both together.

**An unknown key or a value of the wrong shape exits 2, naming the file and the key**, before the run is paid for:

```
  ✗ /home/you/.afi/config.json: unknown key "activ" (did you mean "active"?)
  ✗ /home/you/.afi/config.json: max_tokens must be a whole number from 0 to 4294967295 (got string)
```

Every problem is reported, not just the first, and a file with anything wrong in it applies nothing at all - including the keys that were fine. [`afi sessions`](#subcommands) is refused too, since a file that would not read is also a file that cannot say which sessions there are to list; only `--help` and `--version` still answer. Ignoring what it did not recognize would reproduce the silence the file exists to end, which is also why `"max_tokens": "16000"` is refused rather than read: every reader of that variable parses an integer and keeps its default on anything else. A file that is entirely blank sets nothing and is not an error.

Value checks that already existed still apply. `effort`, `summary`, and a source's `protocol` are closed sets and are refused here. An unusable `approval` still warns and prompts for everything, and a price table with a negative rate still warns and disables cost reporting for the run, both as they do from a variable.

**`--config <path>` or `AFI_CONFIG` reads that file instead of both defaults.** A path that holds no file exits 2, where a default location that holds no file is just a run configured by environment and flags. A blank `AFI_CONFIG` names no file and leaves the defaults alone, since that is what an exported-but-unset shell variable looks like. A `--config` given wrongly - no value, a blank one, or another flag - refuses the run and reads no file at all, so the report names the flag rather than a default file you did not point at.

Three more names have no key, for reasons other than being credentials. `AFI_ENV_FILE` is read before the config file is located, so a key naming it could not take effect. The legacy `AFI_BASE_URL`, `AFI_MODEL`, and `AFI_API_KEY` trio is the flat spelling of one source, and `sources` is the structured one. `AFI_BUILD_*` are set by whoever builds afi, not by whoever runs it.

## System prompt

`--system-prompt-file <path>` (or `AFI_SYSTEM_PROMPT_FILE=<path>`) gives a run its own standing instructions. They reach the model as system content on both protocols - hoisted into `system` on the Messages API, sent as the leading `system` message on an OpenAI-compatible endpoint - which is what separates them from writing the same text into the task prompt file, where it arrives as a user message mixed in with the task.

```bash
afi -f task.md --system-prompt-file ci/review.md --read-only --summary json
```

`--system-prompt-mode` decides what happens to afi's own prompt. A flag wins over its variable, as everywhere else.

| mode                | what the model is told                                          |
| ------------------- | --------------------------------------------------------------- |
| `replace` (default) | the tool-call contract, then your file                           |
| `append`            | afi's whole prompt, then your file                               |

Replacing is what makes the setting worth having. Most of afi's prompt explains how to launch and wait on detached shell commands, which a read-only review job resends on every request and can never act on. `append` is for adding a rule to a run that still wants the rest.

**The tool-call contract survives both modes.** It is four lines describing the `[afi_tool_call]` syntax, and it is a wire format rather than guidance: a model on an endpoint that parses no native tool calls and has not been told that syntax cannot call a tool at all. afi never learns which kind of endpoint it is pointed at - it sends the schemas and reads back either answer - so the alternative to keeping the contract would be refusing every replaced run.

**A prompt that cannot be used exits 2 and names it.** A missing file, an unreadable one, a path that turns out to be a directory, a file that is empty or only whitespace, and a mode that is not one of the two are all refused before the run starts. None of them falls back to the built-in prompt: a job told to send its own instructions and quietly sending afi's is exactly the failure worth avoiding, and an empty file is what a truncated write and an unexpanded template both leave behind.

The flags are stricter than the variables about being given nothing, the way [`--summary-file`](#writing-the-summary-to-a-file) is. `afi --system-prompt-file "$PROMPT"` with `PROMPT` unset is refused; a blank `AFI_SYSTEM_PROMPT_FILE` names no file and is not an error, since that is what an exported-but-unset shell variable looks like in a workflow's env block. A mode set with no file configured does nothing, so a workflow can export `AFI_SYSTEM_PROMPT_MODE` once for jobs that pass a file and jobs that do not.

The [run summary](#run-summary) reports which prompt the run used, so a job's behaviour can be read out of its own output rather than out of the workflow file that was supposed to have configured it. A run that configures nothing sends the bytes afi has always sent, and the Anthropic prompt cache keeps hitting across turns.

## Tool policy

`--read-only`, `--allowed-tools`, and `--disallowed-tools` (or `AFI_READ_ONLY`, `AFI_ALLOWED_TOOLS`, `AFI_DISALLOWED_TOOLS`) bound what a run can reach, independently of approval. A flag wins over its variable, except `--read-only`, which only ever turns the posture on.

```
afi --read-only -f review-prompt.txt
```

That is the whole posture for a job that reads. `--read-only` leaves `read_file` and `list_dir` and denies everything else: the two writers, the shell, and `wait_background`, which deletes the log it hands back. Approval only ever asks about the writers and the shell, so a read-only run has nothing left to prompt for and needs no approval bypass. **It does not need `--yolo`, and pairing the two grants nothing** - the flag would only decide whether afi asks about tools the run can no longer call.

Approval alone cannot express "read but do not write": it decides whether afi *asks*, while the policy decides what exists to ask about. A run that genuinely must write still needs approval settled, and that is the one case for `--yolo`; give it a tool policy too, so "do not ask me" does not also mean "anything at all".

Prefer `--read-only` to spelling out an allow list. It names no tools, so it cannot be mistyped, and it is a denial, so it cannot be widened: `--read-only --allowed-tools run_bash` still leaves `run_bash` blocked. A new mutating tool is covered the day it is added, because the posture and the approval gate read the same list.

An absent or blank list means every tool, so `AFI_ALLOWED_TOOLS=""` from an unset shell variable is not a lockout. A non-empty allow list is exhaustive. Deny always wins, so `--allowed-tools read_file,run_bash --disallowed-tools run_bash` leaves only `read_file`. Names accept commas or whitespace and are case-insensitive. The tools are `read_file`, `write_file`, `edit_file`, `list_dir`, `run_bash`, and `wait_background`.

**A policy that cannot be honoured exits 2 without starting.** A mistyped `--disallowed-tools run_bsah` would otherwise match nothing and leave `run_bash` available while the command line claimed otherwise. A flag with no value is refused the same way, since `--disallowed-tools $DENY` with `DENY` unset would grant everything.

Enforced in two places. Blocked tools are left out of the request, so the model has no schema to call. Dispatch then refuses them regardless, and that is the gate that actually holds: the text protocol parses calls out of prose, so a model can name a tool it was never offered, and the built-in system prompt describes `run_bash` and `wait_background` in prose besides. A blocked call cannot reach the filesystem or the shell even when it arrives. The refusal goes back as a tool result naming the permitted tools, so the turn continues instead of stalling.

`final_answer` is never blockable. It carries the forced-final answer rather than doing anything, so blocking it would strand a run rather than restrict it.

A restricted run shows `tools:` in the status line and lists the permitted set in the [run summary](#run-summary), which also counts every call this policy refused. An unrestricted one shows neither, so the segment appearing is itself the signal.

**This is not a sandbox.** It bounds which afi tools run, not what a permitted command does once started. A permitted `run_bash` can do anything the user can, including editing files, and nothing stops it unsetting these variables for a nested `afi`. Use it to keep a run inside the shape you intended, not to contain something adversarial.

## Reasoning effort

`--effort <level>` (or `AFI_EFFORT`) says how hard the model should think. The levels are `low`, `medium`, `high`, `xhigh`, and `max`, and the flag wins over the variable.

```
afi --effort xhigh -f review-prompt.txt
```

The same level reaches every source in whatever its endpoint calls it:

| endpoint                     | sent as                              | highest level |
| ---------------------------- | ------------------------------------ | ------------- |
| Anthropic Messages API       | `output_config: {"effort": "…"}`     | `max`         |
| OpenRouter                   | `reasoning: {"effort": "…"}`         | `high`        |
| OpenAI                       | `reasoning_effort: "…"`              | `high`        |
| everything else              | nothing                              | -             |

A level above an endpoint's ceiling is capped rather than sent, and a source with no effort control afi knows of - llama.cpp, vLLM, SGLang, Z.ai - gets nothing at all. Both print a line on stderr naming the source, and neither stops the run: a level is a preference the endpoint may simply not have, and dying over it would make the flag unusable in any script that switches source. Only the source the run starts on is reported; `/source` switches to an endpoint with a different ladder without saying so.

Talking to OpenAI's own API also switches the output limit from `max_tokens` to `max_completion_tokens`, since its reasoning models - the only ones `reasoning_effort` applies to - reject the older key outright. Every other endpoint keeps `max_tokens`, the only spelling a self-hosted server implements.

The ceilings above belong to the wire formats, which are stable. **Individual models are stricter, and afi keeps no table of that** - `claude-haiku-4-5` takes no effort at all, and older Opus stops at `high`. A model that rejects a level says so on the first request, which is a clearer answer than a compiled-in list nobody notices going stale.

**An unusable level exits 2 without starting**, whether it came from the flag or the variable. This is the reason to prefer it over hand-writing the same JSON into `EXTRA_BODY`, where a typo is warned about and ignored: a run at an effort nobody asked for finishes normally and looks exactly like one at the right effort, so there is nothing downstream to notice.

`EXTRA_BODY` stays the escape hatch and wins wherever the two would meet. afi never overwrites a level written there by hand, and it never adds one to an object written there either: `{"reasoning":{"max_tokens":2000}}` is left exactly as it is rather than becoming `{"max_tokens":2000,"effort":"high"}`, because OpenRouter documents those two keys as mutually exclusive and afi cannot know which keys any given endpoint pairs that way. Either case prints a line on stderr, and the [run summary](#run-summary) reports whichever level the requests actually carried.

On the Anthropic path one default gives way. `thinking` is sent as `disabled` unless [`AFI_ANTHROPIC_EXTRA_BODY`](#anthropic) says otherwise, and `claude-opus-5` rejects an explicit `disabled` above effort `high`; at `xhigh` and `max` the key is therefore omitted, leaving the model at its own default. Anything explicit in `EXTRA_BODY` is still sent as written, `disabled` included.

**Thinking is charged against `max_tokens`, so the floor moves with it.** Anthropic caps thinking and visible text with one number, and afi's forced-final turn asks for only 2048. Whenever a request may think - because `EXTRA_BODY` turned it on, or because the effort is above `high` - that request's `max_tokens` is floored at 16000 rather than 4096, so the budget cannot go entirely on reasoning and leave nothing to say. Higher effort wants more than the floor, and `AFI_MAX_TOKENS` is how to give it (Anthropic's own guidance is 64000 at `xhigh` and `max`). A turn that ends with no answer at all now prints `FORCED FINAL RETURNED NO ANSWER`, exits 1, and reports `"ok": false` rather than a successful empty answer.

## Run summary

`--summary json` (or `AFI_SUMMARY=json`) prints one JSON object on stdout after a non-interactive run, for CI that needs the result rather than the rendered transcript:

```json
{
  "schema_version": 1,
  "ok": true,
  "error": null,
  "error_kind": null,
  "source": "anthropic",
  "model": "claude-sonnet-5",
  "answer": "…the model's final text…",
  "usage": {
    "input_tokens": 1847,
    "output_tokens": 484,
    "cache_read_tokens": 6837,
    "cache_write_tokens": 2279,
    "reasoning_tokens": 0,
    "total_tokens": 11447,
    "requests": 3,
    "refused_tool_calls": 0,
    "refused_by_policy": 0,
    "refused_by_approval": 0,
    "cost_usd": 0.023398
  },
  "elapsed_secs": 12.17,
  "tools": ["read_file", "write_file", "edit_file", "list_dir", "run_bash", "wait_background"],
  "effort": "xhigh",
  "auth": {
    "mode": "federated",
    "organization_id": "org_...",
    "service_account_id": "svac_...",
    "workspace_id": "wrkspc_...",
    "federation_rule_id": "fdrl_..."
  },
  "system_prompt": {"mode": "builtin", "file": null}
}
```

`schema_version` names the shape of the object, so a field a consumer cannot find is a question about the run rather than about the build. Every summary carries it: on stdout and in the file, from a run that finished and from one [refused before it started](#failure-kinds). It moves only when a summary a working consumer could read stops being readable - a key removed, a key renamed, a type changed, or a meaning that moved under a name that stayed. New keys do not move it, so compare it as "at least 1" and ignore fields you do not know; a consumer that demands an exact version breaks on an upgrade that changed nothing it reads.

**No `schema_version` at all means an afi older than the key**, which is the one version-free shape there will ever be. Before it there was nothing to read but the fields themselves, so dating a build meant probing for one some release had added - an inference that reads a renamed field as an absent one, and that has to be rewritten every time the shape grows.

`answer` is the last assistant message with text, so a review flow can post it directly. Turns that only called tools are skipped.

`tools` is what the run was permitted to call, so an audit of a CI log can confirm the [tool policy](#tool-policy) from the output instead of trusting that the workflow passed the flag it claims to.

`effort` is there for the same reason: it is the level the requests actually carried, read back off the source rather than off the flag, so a capped level reads as the capped one and a level set by hand in `EXTRA_BODY` still shows up. `null` means the run took the endpoint's own default - either nobody asked for a level, or that endpoint has no [effort control](#reasoning-effort) afi knows of.

`usage.refused_tool_calls` is what the run tried anyway, and it comes with the split that makes it readable: `refused_by_policy` for calls the [tool policy](#tool-policy) blocked, `refused_by_approval` for calls the approval gate denied, and the total of the two. A run that refused nothing reports `0` in all three rather than omitting them, so a caller can tell that apart from an afi too old to count them - though `schema_version` is the direct check for that. They live inside `usage`, so a run that never started - one refused over its own [tool policy](#tool-policy) or [summary path](#writing-the-summary-to-a-file) - carries none of them, and `error_kind` is what a caller reads there instead.

**Alert on `refused_by_policy`.** That is the one that means the model reached for a tool the caller had ruled out, which is what matters when the input under review came from outside the trust boundary: `--read-only` guarantees an attempted write failed, and this count is what makes the attempt visible, so a run that was probed and a run that was not stop looking alike.

`refused_by_approval` is a real refusal but a noisier signal, which is why it is a separate number. A run with no terminal and no `--yolo` denies every mutating call by default - see the plain interface above - so an ordinary unattended run that was asked to write reports one here without anything untoward happening. In a `--read-only` run the policy answers first and this stays `0`, which is why an audit reads the policy count.

A tool that ran and failed is an error, not a refusal, and is not counted - a missing file or a command exiting non-zero must not inflate a number worth alerting on. Nor is a call you interrupted with Esc.

The policy count also covers calls thrown away before dispatch could rule on them: a batch whose arguments will not parse, and a forced-final turn answered with a tool - alongside the answer or instead of it. Both discard the whole batch, so a blocked call in one used to leave no trace but a line on stderr, which is exactly what a caller reading the JSON cannot see. The policy reads names, not arguments, so its answer is known even for the call whose arguments were the problem.

**These are attempts, not distinct intentions.** Every blocked call in a discarded batch counts, including one whose own arguments parsed, and a retried batch counts again - so a persistently truncated stream carrying two blocked calls reports six with the default two recoveries, for what a human would call one thing the model wanted. Read the count as "how many times was this run told no", and alert on whether it is zero rather than on how large it is. `AFI_MALFORMED_STREAM_RETRIES` bounds the multiplier.

`auth` is the other half of that posture: which credential the run billed. `mode` is `api_key` for a static key on either protocol, `oauth` for a bearer token minted elsewhere and handed to afi, `federated` for one afi minted itself, `sigv4` for an AWS signature over a stored key ([details](#bedrock)), `sigv4_web_identity` for one over a role afi assumed ([details](#bedrock-without-a-key)), and `none` for a source with no credential configured, which is the local llama.cpp case. The three that have identifiers carry them: the federation ids for `federated`, `region` and `access_key_id` for `sigv4`, and `region`, `role_arn`, and `session_name` for `sigv4_web_identity`. It answers the question that follows `cost_usd` - a job that quietly fell back to a personal key otherwise prints a summary indistinguishable from one that used the service account it was meant to.

It names the credential the tokens were **billed** to, not the one that happens to be active when the run ends. Those differ in a piped session that `/source`-switches after spending: `source` and `model` report where the session finished, while `auth` stays with whoever paid. A session that spent on two sources gets `"auth": null`, since no single credential paid for it - as does a run with no source at all. A run that billed nothing reports the credential it tried, which is what a failed run has to show.

The identifiers are the non-secret ones the [federation](#anthropic) exchange sends, and only those. A rule covering one workspace passes no `ANTHROPIC_WORKSPACE_ID`, so no `workspace_id` comes back here, and a static-key run has nothing to identify at all - both name the mode and stop rather than emitting empty strings. Neither the access token nor the OIDC identity token is ever in the block: this JSON usually ends up as a build artifact, and an artifact carries no masking, so a value redacted in a log would be plain text there.

The five token counts are disjoint and sum to `total_tokens`. They are per-run totals across every billed request, which is what a provider charges for: each turn resends the whole history. `requests` counts those requests - a model turn is one, and so is a compression request, which is why it is not called `turns`. `usage` is `null` rather than a row of zeros when nothing reported any, so a caller can tell a silent provider from a free run - unless the run was refused a call, which afi observed itself and reports either way, with `requests` still `0` to mark the silence.
`system_prompt` is there for the same reason. `mode` is `builtin`, `replace`, or `append`, and `file` is the path the text came from, or `null` for `builtin` - see [System prompt](#system-prompt). The path rather than the text: a prompt can be long, and a job that wants to know what was sent has the file.

The five token counts are disjoint and sum to `total_tokens`. They are per-run totals across every billed request, which is what a provider charges for: each turn resends the whole history. `requests` counts those requests - a model turn is one, and so is a compression request, which is why it is not called `turns`. `usage` is `null` rather than a row of zeros when nothing reported any, so a caller can tell a silent provider from a free run.

`cache_write_tokens` is separate from `cache_read_tokens` and from `input_tokens` because the three are priced differently - Anthropic bills a write above base input and a read far below it, so a cost calculation needs its own rate for each. Only the Anthropic path reports writes; an OpenAI-compatible source reports `0`, as does llama.cpp, whose `timings.cache_n` counts a reused prefix and is therefore a read.

Reporting writes separately re-attributes tokens rather than adding them. The 2279 above used to sit inside `input_tokens`, which is why it comes out of that count and leaves `total_tokens` where it was.

Anthropic prices a 5-minute cache write differently from a 1-hour one and reports them separately. `afi` only ever requests the default TTL, so the single figure here covers every write it can make.

`cost_usd` appears only when you supply rates - see [Cost](#cost) below.

A failed run sets `ok` to false, fills in `error` and `error_kind`, and exits 1.

Both non-interactive entry points report it: `--prompt-file`, and piped stdin with no prompt file. A piped session summarizes the whole session, so `answer` is its last assistant text and `usage` covers every request it made, `/compress` included; any turn failing outright makes the run fail, `/recover` included, and so do a turn with no active source to send it to and an input the session could not read. An interactive TTY session prints nothing extra and always exits 0 — stdout there is the rendered interface, and a human is already reading it.

**Human output moves to stderr** while `--summary json` is set, so stdout holds nothing but the JSON and pipes straight into a parser. Errors go to stderr either way.

## Writing the summary to a file

`--summary-file <path>` (or `AFI_SUMMARY_FILE=<path>`) writes the same object to a path:

```bash
afi -f review-prompt.txt --summary-file "$RUNNER_TEMP/afi-run.json"
jq -r .answer "$RUNNER_TEMP/afi-run.json"
```

**It does not imply `--summary json`.** The two are asked for separately, and stdout keeps the rendered run unless you also ask for the JSON there. That is the reason to want a file: capturing stdout to get the summary costs the readable output, so a workflow that wants both ends up redirecting stdout to a file and reading the answer back out to print it, and the human view becomes a copy of the machine one. Pass both flags to get both channels; they render one object built once, so the file and the pipe cannot disagree about what the run did.

It also takes stdout out of the failure surface. Anything touching the pipe between afi and the parser corrupts the only machine copy - a wrapper, a `tee`, a shell that prints one line of its own. A path is addressed rather than piped, so a caller can upload it as a build artifact without capturing anything.

The file is written to a sibling temp file, flushed, and renamed into place, so a reader that opens the path sees either nothing or one complete object, never the prefix of one still being written. It holds the object and a trailing newline. A rerun replaces it. The temp name is unpredictable and is created with `O_EXCL`, so another local user who can write the directory - a shared runner workspace, a mounted volume - cannot plant a symlink there and have afi truncate and overwrite whatever it points at.

**A path that cannot be written refuses to start**, exiting 2 with the reason, before the run is paid for. A missing directory, an unwritable one, a directory in place of the file, and a path ending in `/` are all found by touching the path at startup; the existing file, if there is one, is left alone until there is a whole object to replace it with. A write that fails anyway - the directory went away mid-run - reports on stderr and exits 1. Falling back to stdout would be no fallback at all, because a caller that named a path is not watching stdout for the answer.

The flag is stricter than the variable about being given nothing. `--summary-file` with no value, with a blank value, or with something that looks like another flag exits 2 the way a broken [tool policy](#tool-policy) does. Both `afi --summary-file $OUT` and `afi --summary-file "$OUT"` with `OUT` unset are refused - the quoted form arrives as an empty argument rather than as no argument, and it is the form a CI script is written in. Either would otherwise exit 0 having written nothing to the path the next step is about to read, or leave a file from an earlier run standing as this run's result. A blank `AFI_SUMMARY_FILE` names no file and is not an error, since that is what an exported-but-unset shell variable looks like.

The same entry points report it as `--summary json` does, and an interactive TTY session writes no file.
## Failure kinds

`error` is the sentence the run printed to stderr, so the log and the JSON never disagree: `HTTP 429: {"type":"error"...}`, or `can't reach http://localhost:8080/v1 - is the server up?`. `error_kind` is what a workflow branches on, and it comes from a closed set:

| kind | what happened | retry? |
| --- | --- | --- |
| `auth` | a credential was missing, unusable, or refused (401, 403) | no |
| `policy` | the tool policy could not be honoured, so the run never started | no |
| `input` | the invocation was wrong - no prompt to read, no source configured, an effort nothing can honour, or a summary file that cannot be written | no |
| `provider_http` | the provider answered with a failing status, or never answered | yes |
| `provider_stream` | the response opened and then broke, or was not a stream at all | yes |
| `timeout` | a request outlived its deadline (including 408 and 504) | yes |
| `no_answer` | the model was reached and billed but never produced an answer | no |
| `internal` | a bug in afi | no |

Retry policy is why the field exists. A rate limit and a rejected credential arrive the same way, as a status and a body; telling them apart here costs nothing, while telling them apart from `error` means matching substrings of a sentence that changes wording, and failing silently when it does.

A rate limit is `provider_http` rather than a kind of its own, since what a caller does about a 429 is what it does about capacity generally. `internal` is not worth retrying either, and it is the only kind that puts the bug on this side.

**`no_answer` is the one failure where the request worked.** The model streamed, the tokens were billed, and nothing usable came out: it looped in its own reasoning until the rescue gave up, the forced final answered with a tool call or an empty string, or its tool arguments stayed unparseable. afi has already spent its own retries by then - the nudges, the recovery sampling, and the forced final are all upstream of this - so another identical attempt is a fresh roll of the dice rather than a fix. It reported `ok: true` before, and `answer` on one of these holds whatever the run last managed to say, which can be an earlier turn's text: `ok` is the gate for posting an answer, not `answer` being non-empty.

**A federated identity exchange refused by its own rule is `auth`, not `provider_http`,** even though it arrives as an HTTP status - most often a 400 or 401 saying the OIDC claims did not satisfy the [federation rule](#anthropic), typically an unprotected ref. An AWS [role assumption](#bedrock-without-a-key) turned down by a trust policy is the same failure and classifies the same way. Retrying either spends the schedule to be refused in the same words. A 429 or a 5xx from the same endpoint stays retryable: that one is the endpoint having a bad minute rather than a verdict on the credential.

`ok: false` always comes with both fields, so a consumer never has to fall back to reading the sentence. A session that failed more than once reports the first kind, since an auth failure repeats on every later turn and the first thing that went wrong is the reason the run did.

**A credential afi sent never comes back in a reported error.** A provider that echoes the request when it refuses one returns the credential inside the body afi then quotes, and the federated path is where that bites: the token exchange posts the OIDC assertion, and afi fetches that assertion from the Actions endpoint itself rather than through the toolkit that would register it for masking, so nothing downstream hides it. The API key and the bearer token are treated the same way, since one path reports all three. Whatever was removed is marked in place - `[redacted OIDC identity token]`, `[redacted API key]`, `[redacted bearer token]`, `[redacted Actions request token]` - so a reader tells a struck credential from a `[truncated]` body that merely ran long. Only the credential goes: the error type and message stay, because a rejected credential and a rate limit are otherwise the same shape.

```bash
afi -f review.txt --summary-file summary.json
case "$(jq -r .error_kind summary.json)" in
  null)                                    post "$(jq -r .answer summary.json)" ;;
  timeout|provider_http|provider_stream)   retry ;;
  no_answer)                               retry_once_then_report ;;
  *)                                       report "$(jq -r .error summary.json)" ;;
esac
```

Exit codes are unchanged: 1 for a failed run, 2 for a refusal to start. A refusal reports itself wherever the summary was asked for, on stdout and in the file, with no `source`, no `model`, and an empty `tools` list - nothing ran, and naming the wide set a mistyped policy resolved to is exactly what refusing avoids. A [tool policy](#tool-policy) that cannot be honoured is `policy`, since a run that started anyway would be wider than the command line asked for. Everything else a run refuses over is `input`: a [summary file](#writing-the-summary-to-a-file) that cannot be written, or an [effort](#reasoning-effort) level no configured source can carry. The summary-file case is reported on stdout alone, since writing the file is what failed.

## Cost

No provider afi speaks to returns a cost. Anthropic's Messages API reports tokens, and so does every OpenAI-compatible endpoint, so the rates have to come from somewhere - and a table compiled into afi is a table nobody notices going stale. You supply it in `AFI_PRICES`, a JSON object mapping model id to USD per million tokens:

```bash
export AFI_PRICES='{
  "claude-sonnet-5": {"input": 3, "output": 15, "cache_read": 0.3, "cache_write": 3.75}
}'
```

The summary then carries `usage.cost_usd`, rounded to the micro-dollar. Without it there is no `cost_usd` key at all - not a null, not a zero, both of which read as "this run was free" to anything summing the field.

The four classes match the token counts they price. `reasoning` is a fifth, optional key; leave it out and reasoning tokens are billed at the `output` rate, which is what every provider here does.

A class you leave unpriced is fine as long as the run spent nothing there - an OpenAI-compatible source reports `0` cache writes on every request, so demanding a write rate would suppress every figure. Spend tokens on an unpriced class, or on a model missing from the table, and `cost_usd` disappears rather than reporting the part it could price.

Model ids match case-insensitively after trimming, and must otherwise be exactly the id afi sends to the provider - what `model` shows in the summary, or what `/source` reports. A mismatch drops the field, which is the point: an absent number is checkable, a wrong one is not.

Rates are read as exact decimals, down to the sixth place - a millionth of a dollar per million tokens, which is a hundredth of a micro-dollar on a ten-million-token run. Exponent notation is read as the number it denotes, so `3e-1` and `0.3` are the same rate.

Four things warn at startup and disable cost reporting for the whole run: a negative rate, a rate finer than the sixth decimal place or too large to hold, a misspelled class key, and a model named twice. The last one counts case and surrounding space as the same id, so `{"M": ..., "m": ...}` is a duplicate - one of the two would otherwise win at random and the bill would change between runs. One unreadable entry is not priced around.

A session that switches models is billed against each model's own rates, so `cost_usd` stays right even though `model` can only name the last one.

## Anthropic

Every other source speaks OpenAI-compatible `/chat/completions`. Anthropic speaks its own Messages API (`POST /v1/messages`): real `tool_use` blocks, adaptive thinking, and a cached system prompt. Sessions, `/compress`, and transcripts are unchanged.

Set one credential and an `anthropic` source registers itself, defaulting to `https://api.anthropic.com` and `claude-sonnet-5`.

| env var                                                                                       | auth mode                          |
| --------------------------------------------------------------------------------------------- | ---------------------------------- |
| `AFI_ANTHROPIC_API_KEY` or `ANTHROPIC_API_KEY`                                                | `x-api-key`                        |
| `AFI_ANTHROPIC_OAUTH_TOKEN` or `ANTHROPIC_AUTH_TOKEN`                                         | pre-minted `Authorization: Bearer` |
| `ANTHROPIC_FEDERATION_RULE_ID` + `ANTHROPIC_ORGANIZATION_ID` + `ANTHROPIC_SERVICE_ACCOUNT_ID` | workload identity federation       |

An API key wins over a bearer token, and a bearer token over federation, matching the official SDKs. The un-prefixed `ANTHROPIC_*` names are read too, so a shell already configured for the Anthropic SDKs or the `ant` CLI works as it stands. Override the endpoint and model with `AFI_ANTHROPIC_BASE_URL` and `AFI_ANTHROPIC_MODEL`, or per switch with `/source anthropic claude-opus-5`.

Those overrides deliberately sit outside the `AFI_SOURCE_*` namespace, which is reserved for sources you define yourself. A bare `AFI_SOURCE_ANTHROPIC_BASE_URL` is enough for afi to auto-discover a source named `anthropic`, and it would come up on the OpenAI-compatible protocol with no credential. Defining a full `AFI_SOURCE_ANTHROPIC_*` block still works, but then set `AFI_SOURCE_ANTHROPIC_PROTOCOL` too.

**Federation.** afi exchanges an OIDC identity token for an access token at `/v1/oauth/token`, then re-mints it near expiry. Add `ANTHROPIC_WORKSPACE_ID` when the rule spans workspaces. The identity token comes from `ANTHROPIC_IDENTITY_TOKEN`, else `ANTHROPIC_IDENTITY_TOKEN_FILE`, else GitHub Actions' OIDC endpoint, so a workflow granting `id-token: write` mints nothing itself:

```yaml
permissions:
  contents: read
  id-token: write
steps:
  - uses: actions/checkout@v7
  - run: afi --read-only -f prompt.txt
    env:
      AFI_ACTIVE: anthropic
      ANTHROPIC_FEDERATION_RULE_ID: fdrl_...
      ANTHROPIC_ORGANIZATION_ID: ...
      ANTHROPIC_SERVICE_ACCOUNT_ID: svac_...
```

The [run summary](#run-summary) reports the rule, organization, service account, and workspace the exchange was configured with, so which budget a job billed is readable from the job's own output.

A read-only job needs nothing else: there is no terminal to answer a prompt, and `--read-only` leaves nothing that would raise one. A job that has to write does need `--yolo` or `AFI_APPROVAL=yolo`, or every write and bash call is denied rather than hanging - pair it with a [tool policy](#tool-policy) so "do not ask me" does not also mean unrestricted.

**Sampling parameters stay off the wire.** Anthropic rejects `temperature`, `top_p`, and `top_k`, and `min_p` and the DRY knobs belong to llama.cpp, so recovery falls back to its prompt-level nudges. `AFI_ANTHROPIC_EXTRA_BODY` accepts `output_config`, `metadata`, `stop_sequences`, and `service_tier`, and drops the rest.

**Thinking is off by default, and `AFI_ANTHROPIC_EXTRA_BODY` turns it on.** The `thinking` key has three states:

| `thinking` in `EXTRA_BODY` | sent as | for |
| ---------------------------- | --------------------------- | -------------------------------------------- |
| absent                       | `{"type": "disabled"}`      | the default; the only shape `claude-haiku-4-5` accepts |
| absent, at effort `xhigh` or `max` | omitted entirely      | `claude-opus-5`, which rejects an explicit `disabled` that high ([details](#reasoning-effort)) |
| `null`                       | omitted entirely            | `claude-fable-5`, which rejects an explicit `disabled` and always thinks |
| an object                    | verbatim                    | `{"type": "adaptive", "display": "summarized"}` |

```bash
AFI_ANTHROPIC_EXTRA_BODY='{"thinking":{"type":"adaptive","display":"summarized"},"output_config":{"effort":"high"}}'
```

Disabled stays the default because it is the one value every current model accepts: `claude-haiku-4-5` rejects adaptive outright, and on `claude-opus-5` disabling thinking is only allowed at effort `high` or below.

`display` decides what you see. The API's default, `omitted`, still thinks and still bills for it but returns empty text, so the reasoning pane stays blank and the turn looks like a long pause. `summarized` streams a readable summary.

**Thinking blocks round-trip.** When a thinking block accompanies a tool call, the API requires the assistant turn echoed back verbatim on the request carrying the tool result — block, text, and signature. afi stores the raw blocks under an `afi_thinking` key on the assistant turn that made the calls, and replays them ahead of the `tool_use` they belong to. That is the only turn that needs them; a plain text answer ends the exchange, so nothing is kept for it. Sessions carry the key (schema stays `afi-1`; a session written by an older afi simply has none), and it is stripped from every OpenAI-protocol request, since it is not part of that wire format.

Three cases lose a block rather than risk the turn: a stream cut before the signature arrived, a `/compress` that sliced away the tool result the reasoning was aimed at, and a request that turns thinking back off. Anthropic validates the whole request, so one unusable block would fail the turn instead of being ignored.

**Reasoning under either spelling now counts toward the cut.** afi reads `reasoning_content` and, when that carries no string, `reasoning`. Only the first was read before, so a source that emits the second - `OpenRouter`, and Bedrock's open-weight models - could never reach `AFI_REASONING_ONLY_CHARS` however long it reasoned. It can now, which is the intended behaviour and a change from previous releases: a turn that reasons past the limit without emitting text is cut and retried. Set `AFI_REASONING_ONLY_CHARS=0` to turn the cut off.

**The reasoning-only cut is off while thinking is on.** `AFI_REASONING_ONLY_CHARS` exists for local models that loop in their scratchpad forever; Anthropic's thinking is server-side and already bounded by `max_tokens`, so cutting one of those turns short would fire on a healthy turn that was about to emit its tool call.

**Other endpoints.** `AFI_SOURCE_<NAME>_PROTOCOL` takes `anthropic`, `anthropic-oauth`, or `aws-bedrock-openai` ([details](#bedrock)). It defaults to `openai`, so existing sources keep working.

## Bedrock

Amazon Bedrock's open-weight models are reached through its OpenAI-compatible `/v1/chat/completions`, so afi sends the same request shape and reads the same SSE stream it does everywhere else. What differs is the credential: Bedrock takes no static key header and signs each request with AWS SigV4 instead.

Set AWS credentials and a Region and a `bedrock` source registers itself, defaulting to `https://bedrock-runtime.<region>.amazonaws.com/v1` and `zai.glm-5`.

| env var                                        | what it does                                                    |
| ------------------------------------------------ | --------------------------------------------------------------- |
| `AWS_ACCESS_KEY_ID` + `AWS_SECRET_ACCESS_KEY` | the signing credential; both required unless a role is assumed  |
| `AWS_REGION`, else `AWS_DEFAULT_REGION`       | names the endpoint host and scopes the signature; always required |
| `AWS_SESSION_TOKEN`                           | sent and signed when set; absent for a long-lived IAM user      |
| `AWS_ROLE_ARN`                                | a role to assume instead of holding a key ([details](#bedrock-without-a-key)) |
| `AWS_ROLE_SESSION_NAME`                       | names that session in CloudTrail, instead of `afi`              |
| `AWS_WEB_IDENTITY_TOKEN_FILE` / `AWS_WEB_IDENTITY_TOKEN` | where the OIDC token to exchange comes from          |
| `AFI_BEDROCK_MODEL`                           | the default model, instead of `zai.glm-5`                       |
| `AFI_BEDROCK_BASE_URL`                        | the endpoint, instead of the one the Region derives             |
| `AFI_BEDROCK_EXTRA_BODY`                      | request-body keys this source should send                       |

These are the variable names every AWS SDK and the `aws` CLI already read, so a shell that can run `aws bedrock` needs nothing else. afi reads them from its own merged environment, which means `~/.env` and `AFI_ENV_FILE` count too. Nothing else is consulted: no shared credentials file, no profile, no instance metadata. Export them, or put them in the env file.

Bedrock hosts many models, so `/source` takes an optional model override:

```
/source bedrock                                  # -> zai.glm-5 (the default)
/source bedrock moonshotai.kimi-k2.5
/source bedrock qwen.qwen3-coder-30b-a3b-v1:0
/source bedrock openai.gpt-oss-20b-1:0
```

Those four are the open-weight models this was built for, and AWS's API-compatibility matrix lists all four as Chat Completions models. Which of them will actually call tools through Bedrock has not been confirmed against the live service; the refusal below is how that resolves, so nothing needs dropping to find out.

**A half-set of credentials refuses the run.** A signature needs a Region and both credential halves at once, so any that are missing are named before the first request:

```
✗ source bedrock signs for Bedrock but AWS_SECRET_ACCESS_KEY is not set
```

Only the source the run starts on is checked. A stray `AWS_ACCESS_KEY_ID` in the shell registers a `bedrock` source nobody switches to, and that costs a run against some other source nothing.

**A model that cannot call tools ends the run, and afi says what that would mean.** An agent turn is tool calls, so a model that cannot make them has nothing to do. Every rejection ends the run either way, and when Bedrock turns down a request that offered tools without explaining itself, the reason is named as the possibility it is:

```
HTTP 400: This model does not support tool use. (if zai.glm-5 cannot call
tools, an agent turn has nothing to dispatch)
```

afi does not claim to know which happened. Bedrock answers a tool-incapable model and a malformed tool schema with the same `ValidationException`, the same status, and the same header, so the only difference is prose, and reading that prose is a trap: afi's own system prompt contains the sentence "does NOT support native tool calls", and any AWS error that quotes the request back carries it. So the hint rides along on an ordinary malformed-request 400 too. AWS's own sentence always leads, and nothing is lost by not guessing: a wrong tool schema and a model that cannot call tools are equally terminal here.

**AWS rejections are told apart.** Every one ends the run with a non-zero exit, and the message AWS wrote is always quoted:

| what happened                                                   | how it reads                                        |
| ----------------------------------------------------------------- | ----------------------------------------------------- |
| `ExpiredTokenException`, `InvalidSignatureException`, and kin  | `AWS rejected the credentials (expired or wrong; afi reads them at startup, so a refresh needs a restart)` |
| `ThrottlingException`, or any 429                              | `AWS throttled the request`                         |
| `AccessDeniedException`                                        | `the account is not entitled to <model> in this Region` |

The kind is read from `x-amzn-errortype` and the status, never from the body, because AWS echoes the request into a validation message and a turn whose prompt discussed throttling would otherwise classify itself. A 429 needs no header, meaning the same thing whoever sent it. Anything else arriving without one did not come from Bedrock's API layer - a proxy or a VPC endpoint refusing on the way - so it stays unclassified and its body is reported as it came, rather than a headerless 403 being called an entitlement problem.

**Anthropic models on Bedrock are out of scope.** afi reaches those directly through the [Anthropic](#anthropic) protocol, which gets prompt caching and thinking blocks that this path does not. Cross-region inference profiles and provisioned throughput are not handled either; point `AFI_BEDROCK_BASE_URL` somewhere else if you need them.

**Other endpoints.** `AFI_SOURCE_<NAME>_PROTOCOL=aws-bedrock-openai` puts any named source on this protocol. It needs no `AFI_SOURCE_<NAME>_BASE_URL` - the Region supplies one - which is the one case where a source is discovered from its `_PROTOCOL` alone.

```bash
AFI_SOURCE_AWS_PROTOCOL=aws-bedrock-openai
AFI_SOURCE_AWS_MODEL=openai.gpt-oss-20b-1:0
```

Bedrock takes `max_completion_tokens` rather than the older `max_tokens`, and afi writes that spelling for this protocol, so `AFI_MAX_TOKENS` and `AFI_BEDROCK_EXTRA_BODY` both reach it under the name Bedrock documents.

Requests are signed for service `bedrock` against `bedrock-runtime`, scoped to `AWS_REGION`. A base url pointed at a differently-named AWS endpoint will not authenticate, and neither will one naming a Region other than the one being signed for.

## Bedrock without a key

A static key pair in the environment is a long-lived credential in every repository that wants to reach Bedrock. Set `AWS_ROLE_ARN` instead and afi assumes that role from an OIDC identity token, the way it already does for [Anthropic](#anthropic): a workflow granting `id-token: write` stores no AWS key at all.

```yaml
permissions:
  contents: read
  id-token: write
steps:
  - uses: actions/checkout@v7
  - run: afi --read-only -f prompt.txt
    env:
      AFI_ACTIVE: bedrock
      AWS_REGION: us-east-1
      AWS_ROLE_ARN: arn:aws:iam::123456789012:role/afi-ci
```

The role's trust policy is what decides whether the workflow may assume it. Register `token.actions.githubusercontent.com` as an IAM OIDC identity provider with `sts.amazonaws.com` as its audience, then condition the role on the `sub` claim - `repo:acme/afi:ref:refs/heads/main`, or whatever the job should be limited to - as AWS's own GitHub Actions documentation describes. afi supplies the token; the account decides what it is worth.

**AWS federates differently from Anthropic, and afi hides the difference.** Anthropic's exchange returns a bearer token that goes in a header. AWS's `sts:AssumeRoleWithWebIdentity` returns a temporary access key, secret, and session token, and those sign requests exactly as a long-lived pair would - so every Bedrock request is SigV4 either way, and only where the credential came from differs.

**The identity token comes from `AWS_WEB_IDENTITY_TOKEN`, else `AWS_WEB_IDENTITY_TOKEN_FILE`, else GitHub Actions' OIDC endpoint,** so a workflow mints nothing itself. The second is the variable every AWS SDK reads, which means a job that already ran `aws-actions/configure-aws-credentials`, or a pod given an EKS service-account identity, needs nothing further. The audience afi asks Actions for is `sts.amazonaws.com`, which is what AWS's setup instructions register. An identity provider created with a different audience is reached by minting the token yourself and passing it in one of those two variables, which skips the Actions endpoint entirely.

**Credentials are re-exchanged before they expire.** STS credentials last an hour by default, and as little as fifteen minutes on a role configured that way, so a session outliving one carries on rather than stopping mid-turn with a 403 that reads like a broken trust policy.

**A static key pair wins over a role.** That is the order every AWS SDK's default credential chain resolves in, and the order afi's own `anthropic` source already uses across its three modes. *Complete* pair: half of one is skipped the same way an SDK skips it, so a misspelled `AWS_SECRET_ACCESS_KEY` does not take down a run that had a perfectly good role to assume. Which one a run actually used is in the [summary](#run-summary): `auth.mode` is `sigv4` for a stored key and `sigv4_web_identity` for an assumed role, so a job that meant to federate and found a stray key in the environment can see that it did.

The assumed-role block reports the role and the session name rather than an access key id. The minted key is re-minted as the run outlives it, so naming one would name whichever session happened to be current when the run ended; the role is the stable answer to whose budget paid.

```json
{"mode": "sigv4_web_identity", "region": "us-east-1",
 "role_arn": "arn:aws:iam::123456789012:role/afi-ci", "session_name": "afi"}
```

**A role that cannot be used refuses the run before it starts,** the same as a half-set of keys:

```
✗ source bedrock assumes an AWS role but no OIDC identity token is available.
  Set AWS_WEB_IDENTITY_TOKEN or AWS_WEB_IDENTITY_TOKEN_FILE, or run inside
  GitHub Actions with `permissions: id-token: write`
✗ source bedrock assumes an AWS role but AWS_ROLE_ARN="afi-ci" is not a role ARN
```

**A refused exchange says which refusal it was, and quotes AWS.** Each of these is an `auth` failure with a non-zero exit, never retried: no second attempt writes a trust policy.

| STS code                                     | how it reads                                          |
| ---------------------------------------------- | ------------------------------------------------------- |
| `AccessDenied`                               | `AWS refused the role assumption: the role's trust policy did not accept the token's claims, or the role does not exist - STS answers both the same way` |
| `InvalidIdentityToken`                       | `AWS would not read the OIDC identity token: no matching identity provider is registered in the account, or the token names an audience other than sts.amazonaws.com` |
| `ExpiredTokenException`                      | `the OIDC identity token expired before it was exchanged` |
| `IDPRejectedClaim`                           | `the identity provider registered on the role rejected the token's claims` |
| `ValidationError`                            | `AWS refused the role-assumption request itself (check AWS_ROLE_ARN and AWS_ROLE_SESSION_NAME)` |

`AccessDenied` names two causes because STS gives one answer to both, deliberately: telling them apart would let anyone holding a GitHub token enumerate an account's roles. Anything else is reported by its code, and AWS's own `<Message>` follows the sentence in every case, so nothing afi failed to classify is lost. A body carrying no code did not come from the STS API layer - a proxy or a VPC endpoint refusing on the way - and stays unclassified for the same reason a headerless Bedrock rejection does.

**Four codes are the endpoint having a bad day, not a credential to fix.** `Throttling`, `ThrottlingException`, and `RequestLimitExceeded` are AWS shedding load; `IDPCommunicationError` is AWS failing to reach the identity provider registered on the role. All four report as `provider_http`, so a job that re-runs on that kind gets its second attempt, and none of them sends anybody auditing a trust policy that was never the problem. STS answers them with a 400 rather than the 429 a modern API would use, which is why the code decides this and not the status; they are reported as a 429 with `STS answered HTTP 400` ahead of AWS's own message, so what came back on the wire is still there. A 429 or a 5xx keeps its status and stays retryable as well.

**The identity token never reaches a reported error.** It is posted in the exchange's form body, so a rejection that echoes the request back carries it, and afi fetched it from the Actions endpoint itself rather than through the toolkit that would have registered it for masking. It is struck as `[redacted OIDC identity token]`, as on the Anthropic path. The AWS session token is struck the same way, since it rides `x-amz-security-token` on every signed request.

The exchange goes to `https://sts.<region>.amazonaws.com/`, derived from `AWS_REGION`; there is no override for it. A VPC-only account with no route to public STS has to mint the token elsewhere and pass it in.

## Subcommands

| subcommand             | what it does                                                                  |
| ---------------------- | ----------------------------------------------------------------------------- |
| `afi`                  | start the REPL                                                                |
| `afi sessions [query]` | list saved sessions, 10 per page (prints + exits) - optional substring filter |

`afi sessions` reads `AFI_SESSIONS_DIR` and `AFI_HOME` from the same resolved settings a run does, so the env file and the [config file](#config-file) move the listing and the runs that saved into it together.

It takes `--page`/`-p` and `--limit`/`-n`, in either spelling, and everything else is the search query. Everything else that is not a `--` word, that is: `afi sessions --config x` used to look for sessions titled `--config x` and find none, while the flag itself quietly did not apply, so a long flag the listing does not have exits 2 naming it. A single dash stays query text, because a title may well start with one.

## Commands

| command                             | what it does                                                    |
| ----------------------------------- | --------------------------------------------------------------- |
| `/source [name] [model]`            | list sources, switch to one, or override its model              |
| `/yolo`                             | toggle auto-approve for writes and bash                         |
| `/approval [level]`                 | show or set risk threshold (`all`/`low`/`medium`/`high`/`yolo`) |
| `/sessions [n]`                     | list recent sessions, or show one in full                       |
| `/save [title]`                     | save the current session (optional custom title)                |
| `/delete [target]`                  | delete a saved session                                          |
| `/compress`                         | summarize older turns into one, keep last 2 verbatim            |
| `/compact`                          | alias for `/compress`                                           |
| `/autocompress [pct\|off\|on]`      | show or set the auto-compress threshold                         |
| `/reset`                            | clear conversation, start a fresh session                       |
| `/clear`                            | alias for `/reset`                                              |
| `/new`                              | alias for `/reset`                                              |
| `/memory save\|remember\|list`      | manage developer memories                                       |
| `/recover [note]`                   | force a low-temp visible checkpoint after a bad stream          |
| `/provider [source] [a,b,...\|off]` | show or set OpenRouter provider-routing order                   |
| `/help`                             | show available commands                                         |
| `/quit`                             | exit                                                            |
