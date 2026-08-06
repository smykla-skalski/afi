// The binary crate is safe Rust; keep it that way (see `lib.rs`).
#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::env;
use std::io::{IsTerminal, stdout};
use std::path::PathBuf;
use std::process;

use afi::cli::cli_sessions_with_style;
use afi::repl::run_repl;
use afi::tools::known_tool_names;

fn main() {
    let args: Vec<String> = env::args().collect();
    let env_map: HashMap<String, String> = env::vars().collect();
    let env_file = env_map
        .get("AFI_ENV_FILE")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".env")));

    // `afi sessions [query]` short-circuits before the REPL - print and exit.
    let stdout = stdout();
    let styled = stdout.is_terminal();
    if cli_sessions_with_style(&args[1..], &env_map, &mut stdout.lock(), styled) {
        return;
    }

    let mut rt = afi::Runtime::build(&args, env_map, env_file.as_deref());

    // A tool policy naming something that does not exist cannot be honoured, and
    // the dangerous reading is silent: `--disallowed-tools run_bsah` would match
    // no tool and leave `run_bash` available. Refuse to start instead.
    let unknown = rt.tool_policy.unknown_names().join(", ");
    if !unknown.is_empty() {
        eprintln!(
            "  \u{2717} unknown tool(s) in --allowed-tools/--disallowed-tools: {unknown}\n    known tools: {}",
            known_tool_names().join(", ")
        );
        process::exit(2);
    }

    // Run the REPL. A failed one-shot run must not exit 0: CI reads the exit
    // code, and reporting success after printing an HTTP error hides the failure.
    if !run_repl(&mut rt) {
        process::exit(1);
    }
}
