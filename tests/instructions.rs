//! What `--instructions` actually puts on the wire, proved against a real process
//! in a real checkout.
//!
//! Asking `Runtime` would prove only that the resolver agrees with itself. The
//! claims worth testing are about the request body and the working directory: that
//! a repository's `AGENTS.md` arrives as system content rather than as a user
//! message, that a run which asks for nothing reads nothing out of the tree it
//! happens to be standing in, and that the summary names what was loaded.
//!
//! The refusals are next door, in `instructions_refused`, which is a separate
//! binary only to stay under the per-file line cap.

use std::fs;
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;

use tempfile::TempDir;

mod common;

use common::endpoint::{Bodies, sent_with_roles, serve, system_sent, text_answer};
use common::{DEEP_RULE, OUTSIDE_RULE, ROOT_RULE, checkout, instruction_paths, repo, run_afi_in};

/// The first request is the only one these runs make.
const FIRST: usize = 0;

/// One server for the runs that need one, sequential so each reads back the body
/// it alone sent.
#[test]
fn the_instructions_a_run_asks_for_are_what_it_sends() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("the fake endpoint must bind");
    let addr = listener
        .local_addr()
        .expect("the endpoint must have an addr");
    let bodies: Bodies = Arc::default();
    let server = serve(listener, &bodies, |_| text_answer("finished"));

    standing_in_a_checkout_and_asking_for_nothing_reads_nothing(addr, &bodies);
    the_walk_sends_both_files_deepest_last(addr, &bodies);
    the_operator_file_leads_the_project_chain(addr, &bodies);
    named_files_are_sent_and_the_tree_is_not_walked(addr, &bodies);
    the_variable_works_and_none_turns_it_off(addr, &bodies);

    drop(server);
}

/// The claim the whole setting rests on. These files are written by whoever wrote
/// the repository, and on a review job that repository is the thing under review,
/// so a run that did not ask for them must not have read them.
fn standing_in_a_checkout_and_asking_for_nothing_reads_nothing(addr: SocketAddr, bodies: &Bodies) {
    let dir = checkout();
    let home = TempDir::new().unwrap();
    run_afi_in(&home, &repo(&dir), Some(addr), &[], &[]);

    let system = system_sent(bodies, FIRST);
    assert!(
        !system.contains(ROOT_RULE),
        "an unconfigured run read the tree it was standing in: {system}"
    );
    assert!(!system.contains("Contents of "), "{system}");
    bodies.lock().unwrap().clear();
}

fn the_walk_sends_both_files_deepest_last(addr: SocketAddr, bodies: &Bodies) {
    let dir = checkout();
    let home = TempDir::new().unwrap();
    run_afi_in(
        &home,
        &repo(&dir).join("crates/api"),
        Some(addr),
        &["--instructions", "project"],
        &[],
    );

    let system = system_sent(bodies, FIRST);
    let root = system
        .find(ROOT_RULE)
        .unwrap_or_else(|| panic!("the root file must be sent: {system}"));
    let deep = system
        .find(DEEP_RULE)
        .unwrap_or_else(|| panic!("the subtree file must be sent: {system}"));
    assert!(root < deep, "the deeper file has to read last: {system}");
    assert!(
        !system.contains(OUTSIDE_RULE),
        "the walk climbed past the repository: {system}"
    );
    assert!(
        system.contains("You are a terminal coding agent"),
        "the instructions add to afi's prompt rather than replacing it: {system}"
    );
    assert_eq!(
        sent_with_roles(bodies, FIRST, &["user"]),
        vec!["review the diff".to_string()],
        "the instructions are system content, not a second user message"
    );
    bodies.lock().unwrap().clear();
}

/// Standing rules of your own, from `$AFI_HOME`, ahead of any repository's.
///
/// Broadest scope first: yours apply to every project, so a repository's answer has
/// to read after them and win where the two disagree.
fn the_operator_file_leads_the_project_chain(addr: SocketAddr, bodies: &Bodies) {
    let dir = checkout();
    let home = TempDir::new().unwrap();
    fs::write(home.path().join("AGENTS.md"), "Always answer in English.")
        .expect("the operator file must write");
    run_afi_in(
        &home,
        &repo(&dir),
        Some(addr),
        &["--instructions", "project"],
        &[],
    );

    let system = system_sent(bodies, FIRST);
    let mine = system
        .find("Always answer in English.")
        .unwrap_or_else(|| panic!("the operator's own file must be sent: {system}"));
    let theirs = system
        .find(ROOT_RULE)
        .unwrap_or_else(|| panic!("the repository's file must be sent: {system}"));
    assert!(mine < theirs, "the operator's file leads: {system}");
    bodies.lock().unwrap().clear();
}

/// What a CI job uses: rules from a path the reviewed branch cannot reach. Naming
/// files is not a walk, so nothing else in the tree is read.
fn named_files_are_sent_and_the_tree_is_not_walked(addr: SocketAddr, bodies: &Bodies) {
    let dir = checkout();
    let home = TempDir::new().unwrap();
    let pinned = home.path().join("review.md");
    fs::write(&pinned, "Apply the policy from this file only.").expect("the file must write");
    run_afi_in(
        &home,
        &repo(&dir),
        Some(addr),
        &["--instructions", pinned.to_str().unwrap()],
        &[],
    );

    let system = system_sent(bodies, FIRST);
    assert!(
        system.contains("Apply the policy from this file only."),
        "{system}"
    );
    assert!(
        !system.contains(ROOT_RULE),
        "naming a file must not also walk the checkout: {system}"
    );
    bodies.lock().unwrap().clear();
}

fn the_variable_works_and_none_turns_it_off(addr: SocketAddr, bodies: &Bodies) {
    let dir = checkout();
    let home = TempDir::new().unwrap();
    run_afi_in(
        &home,
        &repo(&dir),
        Some(addr),
        &[],
        &[("AFI_INSTRUCTIONS", "project")],
    );
    assert!(system_sent(bodies, FIRST).contains(ROOT_RULE));
    bodies.lock().unwrap().clear();

    // `none` is for the run under an operator file or a workflow env block that
    // set `project`, where leaving the variable out is not available.
    run_afi_in(
        &home,
        &repo(&dir),
        Some(addr),
        &["--instructions", "none"],
        &[("AFI_INSTRUCTIONS", "project")],
    );
    let system = system_sent(bodies, FIRST);
    assert!(
        !system.contains(ROOT_RULE),
        "the flag beats the variable, as everywhere else: {system}"
    );
    bodies.lock().unwrap().clear();
}

#[test]
fn the_summary_names_every_file_the_run_loaded() {
    // A job applying this month's rules and a job applying last month's print the
    // same summary without this, and so does one that loaded nothing because a path
    // moved.
    let dir = checkout();
    let home = TempDir::new().unwrap();
    let output = run_afi_in(
        &home,
        &repo(&dir).join("crates/api"),
        None,
        &["--instructions", "project", "--summary", "json"],
        &[],
    );

    let loaded = instruction_paths(&output);
    assert_eq!(loaded.len(), 2, "{loaded:?}");
    assert!(loaded[0].ends_with("repo/AGENTS.md"), "{loaded:?}");
    assert!(loaded[1].ends_with("crates/api/AGENTS.md"), "{loaded:?}");
}

#[test]
fn a_run_that_loaded_none_reports_an_empty_list() {
    // An array on every run, empty included, so a consumer reading it as a list
    // never has to handle a null for the ordinary case.
    let dir = checkout();
    let home = TempDir::new().unwrap();
    let output = run_afi_in(&home, &repo(&dir), None, &["--summary", "json"], &[]);

    assert_eq!(instruction_paths(&output), Vec::<String>::new());
}
