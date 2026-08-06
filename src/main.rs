// The binary crate is safe Rust; keep it that way (see `lib.rs`).
#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::env;
use std::io::{IsTerminal, stdout};
use std::path::PathBuf;
use std::process;

use afi::Runtime;
use afi::cli::{cli_meta, cli_sessions_with_style};
use afi::repl::run_repl;
use afi::summary::{ErrorKind, RunSummary, writable, write_file};
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
    // otherwise), a summary file it could not write, or an effort level nobody
    // could have meant.
    let refusals = rt.refusals();
    if !refusals.is_empty() {
        for refusal in &refusals {
            eprintln!("  \u{2717} {refusal}");
        }
        // The registry is the answer to a mistyped tool name and to nothing
        // else, so it is spelled out only for that refusal.
        let mistyped = rt.tool_policy.unknown_names_message().is_some();
        if mistyped {
            eprintln!("    known tools: {}", known_tool_names().join(", "));
        }
        report_refusal(&rt, &refusals, mistyped);
        process::exit(2);
    }

    // Run the REPL. A failed one-shot run must not exit 0: CI reads the exit
    // code, and reporting success after printing an HTTP error hides the failure.
    if !run_repl(&mut rt) {
        process::exit(1);
    }
}

/// Report a run that refused to start, wherever the caller asked for its summary.
///
/// A caller that asked for one gets one even here. Reporting nothing left it
/// parsing an empty stdout, or reading a summary file still holding a previous
/// run's object, to find out what happened - which is the substring matching the
/// summary exists to end.
fn report_refusal(rt: &Runtime, refusals: &[String], mistyped: bool) {
    if !rt.summary.is_json() && rt.summary_file.is_none() {
        return;
    }
    // A tool policy that cannot be honoured is the graver of the two refusals,
    // since a run that started anyway would have been wider than the command line
    // asked for. A summary file that cannot be written is the invocation naming a
    // path this machine has no answer for.
    let kind = if mistyped || !rt.flag_errors.is_empty() {
        ErrorKind::Policy
    } else {
        ErrorKind::Input
    };
    let reason = refusals.join("; ");
    let summary = RunSummary::refused(&reason, kind).to_json();
    if rt.summary.is_json() {
        println!("{summary}");
    }
    // Skipped when the path is itself what the run refused over: the write would
    // fail in the same words that are already on stderr.
    if let Some(path) = rt
        .summary_file
        .as_deref()
        .filter(|path| writable(path).is_ok())
        && let Err(message) = write_file(path, &summary)
    {
        eprintln!("  \u{2717} {message}");
    }
}
