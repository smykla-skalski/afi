//! Tests for `--help` and `--version` dispatch.

use crate::tools::known_tool_names;
use crate::version::VERSION;

use super::cli_meta;

/// Run `cli_meta` over `args`, returning whether it handled them and what it wrote.
fn run(args: &[&str]) -> (bool, String) {
    let owned: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
    let mut out: Vec<u8> = Vec::new();
    let handled = cli_meta(&owned, &mut out);
    (
        handled,
        String::from_utf8(out).expect("output must be utf-8"),
    )
}

#[test]
fn version_flags_print_the_report() {
    for flag in ["--version", "-V"] {
        let (handled, out) = run(&[flag]);
        assert!(handled, "{flag} must be handled");
        assert!(
            out.starts_with(&format!("afi {VERSION}\n")),
            "{flag}: {out}"
        );
        assert!(out.contains("sha256:"), "{flag}: {out}");
        assert!(out.contains("commit:"), "{flag}: {out}");
        assert!(out.contains("target:"), "{flag}: {out}");
    }
}

#[test]
fn help_flags_print_usage() {
    for flag in ["--help", "-h"] {
        let (handled, out) = run(&[flag]);
        assert!(handled, "{flag} must be handled");
        assert!(
            out.starts_with(&format!("afi {VERSION} - ")),
            "{flag}: {out}"
        );
        assert!(out.contains("\nusage:\n"), "{flag}: {out}");
        assert!(out.contains("-f, --prompt-file"), "{flag}: {out}");
        assert!(out.ends_with('\n'), "{flag}: {out}");
    }
}

#[test]
fn help_documents_every_parsed_flag() {
    let (_, out) = run(&["--help"]);
    // Every flag `parse_args` and the sessions parser accept. A flag that works but
    // is undocumented is indistinguishable from one that does not exist.
    for flag in [
        "--source",
        "--approval",
        "--yolo",
        "--resume",
        "--session",
        "--prompt-file",
        "--summary",
        "--allowed-tools",
        "--disallowed-tools",
        "--version",
        "--help",
        "--limit",
        "--page",
        "sessions",
    ] {
        assert!(out.contains(flag), "help must document {flag}: {out}");
    }
}

#[test]
fn help_lists_the_real_tool_names() {
    let (_, out) = run(&["--help"]);
    // Sourced from the registry, so `--allowed-tools` cannot be documented with a
    // name the policy would reject as unknown.
    for name in known_tool_names() {
        assert!(out.contains(name), "help must list tool {name}: {out}");
    }
}

#[test]
fn no_meta_flag_is_left_for_the_repl() {
    for args in [
        vec![],
        vec!["--yolo"],
        vec!["--source", "anthropic"],
        vec!["sessions"],
        vec!["-f", "prompt.txt"],
    ] {
        let (handled, out) = run(&args);
        assert!(!handled, "{args:?} must not be handled here");
        assert!(out.is_empty(), "{args:?} must print nothing: {out}");
    }
}

#[test]
fn help_wins_over_version() {
    // Both orders, because "whichever came first" would make the output depend on
    // argument order for no reason.
    for args in [
        vec!["--version", "--help"],
        vec!["--help", "--version"],
        vec!["-h", "-V"],
    ] {
        let (handled, out) = run(&args);
        assert!(handled, "{args:?} must be handled");
        assert!(
            out.contains("\nusage:\n"),
            "{args:?} must print usage: {out}"
        );
    }
}

#[test]
fn a_meta_flag_is_found_after_other_arguments() {
    // The flag has to work in the position a user actually types it, which is at
    // the end of a command they were already running.
    let (handled, out) = run(&["--source", "anthropic", "--yolo", "--version"]);
    assert!(handled, "trailing --version must be handled");
    assert!(out.starts_with(&format!("afi {VERSION}\n")), "{out}");

    let (handled, out) = run(&["sessions", "some query", "--help"]);
    assert!(handled, "trailing --help must be handled");
    assert!(out.contains("\nusage:\n"), "{out}");
}

#[test]
fn a_similar_looking_argument_is_not_a_meta_flag() {
    // `-v` is conventionally verbose, and a session titled "help" is a session, so
    // neither may hijack a run.
    for args in [
        vec!["-v"],
        vec!["--versions"],
        vec!["--help-me"],
        vec!["sessions", "help"],
        vec!["sessions", "version"],
    ] {
        let (handled, out) = run(&args);
        assert!(!handled, "{args:?} must not be handled: {out}");
    }
}
