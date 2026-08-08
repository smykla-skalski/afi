//! `--help` and `--version`, which answer and exit without starting a run.

use std::io::Write;

use crate::tools::known_tool_names;
use crate::version::{VERSION, report_current};

#[cfg(test)]
mod tests;

/// Usage text. Kept in step with the flag table in `docs/reference.md`.
const USAGE: &str = "\
usage:
  afi [flags]                    start the REPL
  afi -f <path> [flags]          run one prompt non-interactively, then exit
  afi sessions [query] [flags]   list saved sessions, then exit

flags:
  --config <path>                read settings from this file instead of the defaults
  --source <name>                start on a specific source
  --approval <mode>              all | low | medium | high | yolo
  --yolo                         never prompt; auto-approve every tool call
  --resume [target]              resume a saved session (bare = most recent)
  --session <id>                 attach a fresh run to a specific session id
  -f, --prompt-file <path>       one-shot mode; '-' reads the prompt from stdin
  --summary json                 print a machine-readable run summary on stdout
  --summary-file <path>          also write that summary to a path
  --effort <level>               low | medium | high | xhigh | max
  --system-prompt-file <path>    send these standing instructions to the model
  --system-prompt-mode <mode>    replace (default) | append, against the built-in prompt
  --instructions <value>         project | none | a comma-separated list of paths
  --budget-usd <usd>             stop the run once it has spent this much
  --read-only                    deny every tool that can change anything
  --allowed-tools <a,b>          only these tools may be called
  --disallowed-tools <a,b>       these tools may not be called
  -V, --version                  print the version, the build, and this binary's digest
  -h, --help                     print this message

sessions flags:
  -n, --limit <n>                sessions per page (default 10)
  -p, --page <n>                 which page to show (default 1)
";

/// Handle `--help`/`-h` and `--version`/`-V`.
///
/// Returns `true` when one of them was present, in which case the output is
/// already written and the caller must exit without starting a run.
///
/// Checked before every other argument, so `afi sessions --help` explains itself
/// rather than hunting for a session titled `--help`, and so neither flag depends
/// on a readable env file, a resolvable source, or an honourable tool policy.
pub fn cli_meta<W: Write>(args: &[String], out: &mut W) -> bool {
    let mut version = false;
    for arg in args {
        match arg.as_str() {
            // Help wins wherever both appear, matching every other CLI.
            "--help" | "-h" => {
                write_all(out, &help_text());
                return true;
            }
            "--version" | "-V" => version = true,
            _ => {}
        }
    }
    if version {
        write_all(out, &report_current());
    }
    version
}

/// The usage text, prefixed with the version and followed by the tool names
/// `--allowed-tools` accepts.
///
/// The tool list comes from the registry rather than a second hand-written copy,
/// which would be free to disagree with what the policy actually enforces.
fn help_text() -> String {
    let tools = known_tool_names().join(", ");
    format!(
        "afi {VERSION} - a deliberately tiny coding agent for self-hosted or remote models.\n\
         \n\
         {USAGE}\
         \n\
         tools:\n\
         \x20 {tools}\n\
         \n\
         environment variables and slash commands: docs/reference.md\n"
    )
}

/// Write the whole message, discarding an I/O error.
///
/// `afi --help | head -5` closes the pipe early, and dying on that broken pipe
/// would turn a normal shell idiom into a failure.
fn write_all<W: Write>(out: &mut W, text: &str) {
    let _ = out.write_all(text.as_bytes());
}
