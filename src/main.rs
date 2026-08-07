// The binary crate is safe Rust; keep it that way (see `lib.rs`).
#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::env;
use std::io::{IsTerminal, stdout};
use std::process;

use afi::Runtime;
use afi::cli::{Listing, cli_meta, cli_sessions_with_style};
use afi::repl::run_repl;
use afi::summary::{ErrorKind, RunError, RunSummary, writable, write_file};
use afi::tools::known_tool_names;

fn main() {
    let args: Vec<String> = env::args().collect();
    let env_map: HashMap<String, String> = env::vars().collect();

    let stdout = stdout();

    // `--help` and `--version` answer first, so neither depends on an env file
    // loading, a source resolving, or a tool policy being honourable. Ahead of
    // `sessions` too, or `afi sessions --help` would search for a session.
    if cli_meta(&args[1..], &mut stdout.lock()) {
        return;
    }

    // The env file and the config files, before anything reads a setting. Ahead
    // of `sessions` because that resolves its directory from `AFI_HOME`, which a
    // config file can set: reading the files afterwards would list one directory
    // while runs saved into another.
    let (env_map, settings) = afi::Runtime::resolve_env(&args, env_map);

    // `afi sessions [query]` short-circuits before the REPL - print and exit.
    // Skipped when a config file would not read: the listing resolves its
    // directory from the same settings a run does, and a file that set `home` and
    // then failed would have it quietly list the default one instead. Falling
    // through reaches the refusal below, which reports itself properly.
    let styled = stdout.is_terminal();
    if settings.refusals().is_empty() && listed_sessions(&args, &env_map, styled) {
        return;
    }

    let mut rt = afi::Runtime::build_resolved(&args, env_map, &settings);

    // Anything else the run was told to do that it cannot: a tool policy that
    // would degrade into a wider grant than was asked for (`--disallowed-tools
    // run_bsah` matches no tool, a bare `--disallowed-tools` sets none at all,
    // and either leaves `run_bash` available while the command line says
    // otherwise), a summary file it could not write, or an effort level nobody
    // could have meant.
    let refusals = rt.refusals();
    if let Some(first) = refusals.first() {
        for refusal in &refusals {
            eprintln!("  \u{2717} {}", refusal.message);
        }
        // The registry is the answer to a mistyped tool name and to nothing
        // else, so it is spelled out only for that refusal.
        if rt.tool_policy.unknown_names_message().is_some() {
            eprintln!("    known tools: {}", known_tool_names().join(", "));
        }
        report_refusal(&rt, &refusals, first.kind);
        process::exit(2);
    }

    // Run the REPL. A failed one-shot run must not exit 0: CI reads the exit
    // code, and reporting success after printing an HTTP error hides the failure.
    if !run_repl(&mut rt) {
        process::exit(1);
    }
}

/// Answer `afi sessions [query]`, when that is what argv asked for.
///
/// Returns whether the listing answered, so the caller can stop reading argv. An
/// argument the listing cannot honour exits here rather than being handed back: a
/// listing has no summary to carry the reason and no runtime to ask for one, so
/// the report on stderr is the whole of it.
fn listed_sessions(args: &[String], env: &HashMap<String, String>, styled: bool) -> bool {
    match cli_sessions_with_style(&args[1..], env, &mut stdout().lock(), styled) {
        Listing::Printed => true,
        Listing::NotAsked => false,
        Listing::Refused(why) => {
            for refusal in &why {
                eprintln!("  \u{2717} {refusal}");
            }
            process::exit(2);
        }
    }
}

/// Report a run that refused to start, wherever the caller asked for its summary.
///
/// A caller that asked for one gets one even here. Reporting nothing left it
/// parsing an empty stdout, or reading a summary file still holding a previous
/// run's object, to find out what happened - which is the substring matching the
/// summary exists to end.
///
/// `kind` is the first refusal's, matching the first-wins rule a failed run
/// already uses: the reason a run went wrong is the first thing that went wrong.
fn report_refusal(rt: &Runtime, refusals: &[RunError], kind: ErrorKind) {
    if !rt.summary.is_json() && rt.summary_file.is_none() {
        return;
    }
    let reason = refusals
        .iter()
        .map(|refusal| refusal.message.as_str())
        .collect::<Vec<_>>()
        .join("; ");
    let summary = RunSummary::refused(&reason, kind).to_json();
    if rt.summary.is_json() {
        println!("{summary}");
    }
    // Skipped when the path is itself what the run refused over: the write would
    // fail in the same words that are already on stderr. Probed again rather than
    // inferred from the refusal list, so a later refusal of the same kind cannot
    // silently suppress a write to a path that is perfectly good.
    if let Some(path) = rt
        .summary_file
        .as_deref()
        .filter(|path| writable(path).is_ok())
        && let Err(message) = write_file(path, &summary)
    {
        eprintln!("  \u{2717} {message}");
    }
}
