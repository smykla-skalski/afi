//! Subtree instructions, proved against a real process reading real files.
//!
//! The startup walk goes up, so a run started at a repository root never sees
//! `crates/api/AGENTS.md`. This is the other half: the file arrives when the model
//! reads into that subtree. The state behind it is process-wide, which is why these
//! claims - once per directory, only inside the project, only for a run that asked
//! for a walk - are asserted here rather than in unit tests sharing one accumulator.
//!
//! What happens to those blocks when the history is rewritten - `/reset`, `/compress`,
//! `--resume` - lives in `instructions_history`, a separate binary only to stay under
//! the per-file line cap.

use std::fs;
use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::process::Output;
use std::sync::Arc;

use tempfile::TempDir;

mod common;

use common::endpoint::{Bodies, LAST, sent_with_roles, serve, tool_call_then_text};
use common::{
    DEEP_RULE as API_RULE, OUTSIDE_RULE, instruction_paths, repo, spawn_afi_in, workspace,
};

/// Run afi from `cwd` against `addr`, feeding one prompt and quitting.
fn run_afi(home: &TempDir, cwd: &Path, addr: SocketAddr, extra: &[&str]) -> Output {
    let mut args = vec!["-f", "-"];
    args.extend_from_slice(extra);
    spawn_afi_in(home, cwd, Some(addr), &args, &[], "read the api crate\n")
}

/// Every tool result in the run's final history, in order.
///
/// The last request rather than all of them: every turn resends the whole history, so
/// folding the bodies together counts each result once per turn that followed it -
/// and "the rules rode once" would fail on a run that behaved. Text-protocol
/// observations arrive as a user message, so both roles count.
fn tool_results(bodies: &Bodies) -> Vec<String> {
    sent_with_roles(bodies, LAST, &["tool", "user"])
}

#[test]
fn a_read_into_a_subtree_brings_its_rules_with_the_result() {
    let dir = workspace();
    let home = TempDir::new().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").expect("the fake endpoint must bind");
    let addr = listener.local_addr().expect("an addr");
    let bodies: Bodies = Arc::default();
    // Two reads of the same file: the rules ride on the first result and not the
    // second, which is the "once per directory" half.
    let server = serve(listener, &bodies, |seen| {
        tool_call_then_text(
            seen,
            "read_file",
            &serde_json::json!({"path": "crates/api/src/lib.rs"}),
            2,
        )
    });

    let output = run_afi(
        &home,
        &repo(&dir),
        addr,
        &["--instructions", "project", "--summary", "json"],
    );
    drop(server);

    let results = tool_results(&bodies);
    let carrying: Vec<&String> = results
        .iter()
        .filter(|text| text.contains(API_RULE))
        .collect();
    assert_eq!(
        carrying.len(),
        1,
        "the subtree rules ride once, not on every read: {results:#?}"
    );
    assert!(
        carrying[0].contains("Contents of "),
        "and name the file they came from: {}",
        carrying[0]
    );
    assert!(
        carrying[0].contains("pub fn go()"),
        "appended to the tool's own output rather than replacing it: {}",
        carrying[0]
    );

    // Both halves in the summary, startup first, in the order they were sent.
    let paths = instruction_paths(&output);
    assert_eq!(paths.len(), 2, "{paths:?}");
    assert!(paths[0].ends_with("AGENTS.md") && !paths[0].contains("crates"));
    assert!(paths[1].ends_with("crates/api/AGENTS.md"), "{paths:?}");
}

#[test]
fn a_run_that_named_its_files_gets_nothing_from_the_tree() {
    // Naming files pins exactly what a job sends, and a rule arriving mid-session
    // out of the tree under review is the thing pinning exists to prevent.
    let dir = workspace();
    let home = TempDir::new().unwrap();
    let pinned = home.path().join("review.md");
    fs::write(&pinned, "Apply the policy from this file only.").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").expect("the fake endpoint must bind");
    let addr = listener.local_addr().expect("an addr");
    let bodies: Bodies = Arc::default();
    let server = serve(listener, &bodies, |seen| {
        tool_call_then_text(
            seen,
            "read_file",
            &serde_json::json!({"path": "crates/api/src/lib.rs"}),
            1,
        )
    });

    let output = run_afi(
        &home,
        &repo(&dir),
        addr,
        &[
            "--instructions",
            pinned.to_str().unwrap(),
            "--summary",
            "json",
        ],
    );
    drop(server);

    let results = tool_results(&bodies);
    assert!(
        !results.iter().any(|text| text.contains(API_RULE)),
        "a pinned run read the tree anyway: {results:#?}"
    );
    assert_eq!(
        instruction_paths(&output).len(),
        1,
        "only the file it named"
    );
}

#[test]
fn a_relative_path_climbing_out_with_dot_dot_brings_nothing_back() {
    // The hole the absolute-path case below cannot find. `canonicalize` needs the
    // whole path to exist, so a target that does not - which is most of what a model
    // mistypes - used to fall back to the lexical join, and `Path::starts_with` reads
    // `<repo>/src/../../..` as inside `<repo>`. One such call pulled the parent's and
    // the grandparent's AGENTS.md in and reported them as this project's rules.
    let dir = workspace();
    let home = TempDir::new().unwrap();
    // `checkout` already puts a file one level above the repo; add one further up.
    let above = repo(&dir)
        .parent()
        .and_then(Path::parent)
        .expect("two levels up")
        .join("AGENTS.md");
    fs::write(&above, "TOP LEVEL RULE: not this project's business.").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").expect("the fake endpoint must bind");
    let addr = listener.local_addr().expect("an addr");
    let bodies: Bodies = Arc::default();
    let server = serve(listener, &bodies, |seen| {
        tool_call_then_text(
            seen,
            "read_file",
            &serde_json::json!({"path": "crates/../../../nope/x"}),
            1,
        )
    });

    let output = run_afi(
        &home,
        &repo(&dir),
        addr,
        &["--instructions", "project", "--summary", "json"],
    );
    drop(server);

    let results = tool_results(&bodies);
    for leaked in ["TOP LEVEL RULE", OUTSIDE_RULE] {
        assert!(
            !results.iter().any(|text| text.contains(leaked)),
            "a `..` path climbed out and pulled in {leaked:?}: {results:#?}"
        );
    }
    assert_eq!(
        instruction_paths(&output).len(),
        1,
        "only the project's own file: {:?}",
        instruction_paths(&output)
    );
}

#[test]
fn a_repository_whose_rules_live_only_in_a_subtree_still_gets_them() {
    // The startup walk finds nothing at or above the root, which used to leave the
    // subtree half unarmed for the whole session - so a monorepo with per-crate rules
    // and no root file got neither half.
    let dir = workspace();
    fs::remove_file(repo(&dir).join("AGENTS.md")).expect("the root file must go");
    let home = TempDir::new().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").expect("the fake endpoint must bind");
    let addr = listener.local_addr().expect("an addr");
    let bodies: Bodies = Arc::default();
    let server = serve(listener, &bodies, |seen| {
        tool_call_then_text(
            seen,
            "read_file",
            &serde_json::json!({"path": "crates/api/src/lib.rs"}),
            1,
        )
    });

    let output = run_afi(
        &home,
        &repo(&dir),
        addr,
        &["--instructions", "project", "--summary", "json"],
    );
    drop(server);

    assert!(
        tool_results(&bodies).iter().any(|t| t.contains(API_RULE)),
        "an empty startup walk must not disarm the subtree half: {:#?}",
        tool_results(&bodies)
    );
    assert_eq!(instruction_paths(&output).len(), 1);
}

#[test]
fn a_read_outside_the_project_brings_nothing_back() {
    // The boundary the startup walk keeps, kept here too: a call on `$HOME` or
    // `/etc` must not turn whatever standing instructions live there into this
    // project's rules.
    let dir = workspace();
    let home = TempDir::new().unwrap();
    // A third tree, so neither the project nor `$AFI_HOME` accounts for it.
    let elsewhere = TempDir::new().unwrap();
    fs::write(
        elsewhere.path().join("AGENTS.md"),
        "Rules from outside the project.",
    )
    .unwrap();
    fs::write(elsewhere.path().join("notes.txt"), "just a file\n").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").expect("the fake endpoint must bind");
    let addr = listener.local_addr().expect("an addr");
    let bodies: Bodies = Arc::default();
    let outside = elsewhere.path().join("notes.txt");
    let server = serve(listener, &bodies, move |seen| {
        tool_call_then_text(
            seen,
            "read_file",
            &serde_json::json!({"path": outside.to_str().expect("a path")}),
            1,
        )
    });

    let output = run_afi(
        &home,
        &repo(&dir),
        addr,
        &["--instructions", "project", "--summary", "json"],
    );
    drop(server);

    let results = tool_results(&bodies);
    assert!(
        !results
            .iter()
            .any(|text| text.contains("Rules from outside the project")),
        "a read outside the project pulled its rules in: {results:#?}"
    );
    // Only the project's own root file, found by the startup walk.
    assert_eq!(
        instruction_paths(&output).len(),
        1,
        "{:?}",
        instruction_paths(&output)
    );
}
