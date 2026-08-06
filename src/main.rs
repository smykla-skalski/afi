// The binary crate is safe Rust; keep it that way (see `lib.rs`).
#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::env;
use std::io::{IsTerminal, stdout};
use std::path::PathBuf;
use std::process;

use afi::cli::{cli_meta, cli_sessions_with_style};
use afi::repl::run_repl;
use afi::tools::known_tool_names;

fn main() {
    let args: Vec<String> = env::args().collect();
    let env_map: HashMap<String, String> = env::vars().collect();
    let env_file = env_map
        .get("AFI_ENV_FILE")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".env")));

    let stdout = stdout();

    // `--help` and `--version` answer first, so neither depends on an env file
    // loading, a source resolving, or a tool policy being honourable. Ahead of
    // `sessions` too, or `afi sessions --help` would search for a session.
    if cli_meta(&args[1..], &mut stdout.lock()) {
        return;
    }

    // `afi sessions [query]` short-circuits before the REPL - print and exit.
    let styled = stdout.is_terminal();
    if cli_sessions_with_style(&args[1..], &env_map, &mut stdout.lock(), styled) {
        return;
    }

    let mut rt = afi::Runtime::build(&args, env_map, env_file.as_deref());

    // Anything the run was told to do that it cannot: a tool policy that would
    // degrade into a wider grant than was asked for (`--disallowed-tools
    // run_bsah` matches no tool, a bare `--disallowed-tools` sets none at all,
    // and either leaves `run_bash` available while the command line says
    // otherwise), or a summary file it could not write.
    let refusals = rt.refusals();
    if !refusals.is_empty() {
        for refusal in &refusals {
            eprintln!("  \u{2717} {refusal}");
        }
        // The registry is the answer to a mistyped tool name and to nothing
        // else, so it is spelled out only for that refusal.
        if rt.tool_policy.unknown_names_message().is_some() {
            eprintln!("    known tools: {}", known_tool_names().join(", "));
        }
        process::exit(2);
    }

    // Run the REPL. A failed one-shot run must not exit 0: CI reads the exit
    // code, and reporting success after printing an HTTP error hides the failure.
    if !run_repl(&mut rt) {
        process::exit(1);
    }
}
