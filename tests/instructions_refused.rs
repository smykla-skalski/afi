//! What `--instructions` refuses, and who is allowed to turn it on.
//!
//! Every case here would otherwise be a run that reports following the rules it
//! was pointed at while following none of them. Split from `instructions`, which
//! covers what reaches the wire, only to stay under the per-file line cap; both
//! drive a real process from a real checkout built by the same helper.

use std::fs;

use afi::repl::banner;
use serde_json::Value;
use tempfile::TempDir;

mod common;

use common::endpoint::{endpoint, reads_the_api_crate};
use common::{
    ROOT_RULE, checkout, repl_afi_in, repo, run_afi_in, stderr_of, summary_of, workspace,
};

/// Each of these has to exit 2 and name what it could not use.
#[test]
fn instructions_that_cannot_be_used_refuse_the_run() {
    let dir = checkout();
    let home = TempDir::new().unwrap();
    let missing = home.path().join("absent.md");
    let missing = missing.to_str().unwrap();
    let empty = home.path().join("empty.md");
    fs::write(&empty, "  \n\n").expect("the file must write");
    let empty = empty.to_str().unwrap();

    for (label, args, expected) in [
        ("a missing file", vec!["--instructions", missing], missing),
        ("an empty file", vec!["--instructions", empty], empty),
        (
            "a value naming nothing",
            vec!["--instructions", ",,"],
            "none",
        ),
        ("no value at all", vec!["--instructions"], "--instructions"),
        (
            "a value that is another flag",
            vec!["--instructions", "--yolo"],
            "--instructions",
        ),
        (
            "an unset shell variable, quoted",
            vec!["--instructions", ""],
            "--instructions",
        ),
    ] {
        let output = run_afi_in(&home, &repo(&dir), None, &args, &[]);
        let stderr = stderr_of(&output);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{label} must refuse the run, stderr: {stderr}"
        );
        assert!(
            stderr.contains(expected),
            "{label} must name {expected:?}, said: {stderr}"
        );
    }
}

#[test]
fn a_blank_variable_is_not_a_refusal() {
    // An exported-but-unset variable is how a workflow leaves the setting off for
    // one job, so it has to mean "no instructions" rather than "stop".
    let dir = checkout();
    let home = TempDir::new().unwrap();
    let output = run_afi_in(
        &home,
        &repo(&dir),
        None,
        &["--summary", "json"],
        &[("AFI_INSTRUCTIONS", "")],
    );

    assert_ne!(output.status.code(), Some(2), "{}", stderr_of(&output));
    assert_eq!(
        summary_of(&output)["instructions"],
        Value::Array(Vec::new())
    );
}

#[test]
fn a_project_config_file_cannot_turn_the_walk_on() {
    // The circular case: a file in the working tree deciding that the working
    // tree's own standing instructions get read into the agent's prompt.
    let dir = checkout();
    let home = TempDir::new().unwrap();
    let config = repo(&dir).join(".afi");
    fs::create_dir_all(&config).expect("the config dir must write");
    fs::write(config.join("config.json"), r#"{"instructions": "project"}"#)
        .expect("the config must write");

    let output = run_afi_in(&home, &repo(&dir), None, &[], &[]);
    let stderr = stderr_of(&output);
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("instructions")
            && stderr.contains("cannot be set by a file in the working directory"),
        "{stderr}"
    );
}

#[test]
fn the_instructions_command_lists_what_was_loaded_with_its_sizes() {
    // The first thing to reach for when the model ignores a rule the repository
    // states: a file that was never found, a subtree file above where afi started,
    // and a rule the model just did not follow are otherwise indistinguishable.
    let dir = checkout();
    let home = TempDir::new().unwrap();
    let output = repl_afi_in(
        &home,
        &repo(&dir),
        None,
        &["--instructions", "project"],
        "/instructions\n/quit\n",
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("AGENTS.md"), "{stdout}");
    assert!(
        stdout.contains(&format!("{} bytes", ROOT_RULE.len())),
        "the bytes that were sent, not the file's size now: {stdout}"
    );

    // And says so plainly when there are none, rather than printing an empty list
    // that reads like a bug.
    let output = repl_afi_in(&home, &repo(&dir), None, &[], "/instructions\n/quit\n");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("No project instructions loaded"),
        "{stdout}"
    );
    assert!(stdout.contains("--instructions project"), "{stdout}");
}

#[test]
fn the_status_line_counts_both_halves_and_only_when_there_are_any() {
    // The segment appearing is itself the signal, so an unconfigured session's status
    // line has to be unchanged.
    let plain = common::build(&["afi"], &[]);
    assert!(!banner(&plain).contains("instructions:"));

    let home = TempDir::new().unwrap();
    let rules = home.path().join("rules.md");
    fs::write(&rules, "Never touch the lockfile.").expect("the file must write");
    let loaded = common::build(&["afi", "--instructions", rules.to_str().unwrap()], &[]);
    assert!(
        banner(&loaded).contains("instructions:1"),
        "{}",
        banner(&loaded)
    );
}

#[test]
fn the_status_line_follows_a_subtree_file_loaded_mid_session() {
    // The count is rendered from `nested::sent`, which grows when the model reads
    // into a subtree - so the header has to be re-rendered after a turn or it reports
    // one number while `/instructions` reports another.
    let dir = workspace();
    let home = TempDir::new().unwrap();
    let (addr, _bodies) = endpoint(reads_the_api_crate);

    let output = repl_afi_in(
        &home,
        &repo(&dir),
        Some(addr),
        &["--instructions", "project"],
        "read it\n/quit\n",
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("instructions:2"),
        "the status line has to follow the on-demand load: {stdout}"
    );
}
