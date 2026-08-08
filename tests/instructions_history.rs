//! What happens to a subtree instruction block when the history that carried it is
//! rewritten.
//!
//! Every one of these rides in a tool result, so `/reset`, `/compress`, and `--resume`
//! each do something different to it: emptied, kept-or-folded, replayed. The rule is
//! that the model's view and what `/instructions` reports must agree afterwards, and
//! that no block is ever sent twice. Split from `instructions_nested` only to stay
//! under the per-file line cap.

use std::fs;

use tempfile::TempDir;

mod common;

use common::endpoint::{LAST, endpoint, reads_the_api_crate, sent_with_roles, tool_results};
use common::{DEEP_RULE as API_RULE, repl_afi_in, repo, session_of, workspace};

#[test]
fn a_reset_history_is_told_the_subtree_rules_again() {
    // Every subtree block rides in a tool result, so `/reset` - which rebuilds the
    // history from the system message alone - takes them out of the conversation. The
    // once-per-directory memory has to go with them, or the rules are gone for the
    // rest of the session while `/instructions` still reports them as sent.
    //
    // `API_RULE` lives in `crates/api/AGENTS.md`, which the startup walk never reads
    // (it walks up, not down), so every appearance of it on the wire came from the
    // subtree loader.
    let dir = workspace();
    let home = TempDir::new().unwrap();
    let (addr, bodies) = endpoint(reads_the_api_crate);

    repl_afi_in(
        &home,
        &repo(&dir),
        Some(addr),
        &["--instructions", "project"],
        "read it\n/reset\nread it again\n/quit\n",
    );

    // The last request is the second read's, in a history the reset emptied. Tool
    // results only, not the shared `tool_results`, which also takes user messages: a
    // bare `any` over both roles would pass if the block moved off the tool result it
    // is specified to ride on and onto the observation turn.
    let after = sent_with_roles(&bodies, LAST, &["tool"]);
    assert!(
        after.iter().any(|text| text.contains(API_RULE)),
        "a fresh history was never told the subtree rules again: {after:#?}"
    );
}

#[test]
fn a_compressed_history_is_told_again_about_a_rule_it_lost() {
    // The other side of the test below. The fold keeps only the most recent turns, so a
    // block older than that window leaves the conversation - and the run has to notice,
    // or the rules are gone for good while `/instructions` still reports them as sent.
    let dir = workspace();
    let home = TempDir::new().unwrap();
    let (addr, bodies) = endpoint(reads_the_api_crate);

    // Two turns before the fold, so the block-carrying result is older than the kept
    // window and is folded away rather than surviving in it.
    let output = repl_afi_in(
        &home,
        &repo(&dir),
        Some(addr),
        &["--instructions", "project"],
        "read it\nand again\n/compress\nread it once more\n/quit\n",
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Compressed context"),
        "the fold has to have happened: {stdout}"
    );
    let sent = tool_results(&bodies);
    assert!(
        sent.iter().any(|text| text.contains(API_RULE)),
        "a rule the fold dropped has to be offered again: {sent:#?}"
    );
}

#[test]
fn a_compressed_history_is_not_told_a_rule_it_still_holds() {
    // The other side of the reset above. `/compress` keeps the most recent turns
    // verbatim, so a block appended to one of them is still in the conversation -
    // forgetting it there would re-send the same text and hand back the byte budget
    // meant to bound it. A 32 KiB cap is no cap if every fold doubles what it counts.
    let dir = workspace();
    let home = TempDir::new().unwrap();
    let (addr, bodies) = endpoint(reads_the_api_crate);

    let output = repl_afi_in(
        &home,
        &repo(&dir),
        Some(addr),
        &["--instructions", "project"],
        "read it\n/compress\nread it again\n/quit\n",
    );

    // Without this the rest is vacuous: "too few turns", an empty summary, or a
    // broken non-stream path all leave one copy of the rule and no fold at all.
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Compressed context"),
        "the fold has to have happened: {stdout}"
    );
    let carrying = tool_results(&bodies)
        .iter()
        .filter(|text| text.contains(API_RULE))
        .count();
    assert_eq!(
        carrying,
        1,
        "the fold kept the block, so it must not be sent a second time: {:#?}",
        tool_results(&bodies)
    );
}

#[test]
fn a_resume_does_not_re_adopt_the_startup_walk() {
    // The system message carries the startup walk's own blocks, so a resume that read
    // its record off the message text found them again and reported the root file
    // twice - once bare, once "carried in" - and charged its bytes twice, which could
    // starve the budget and refuse a genuine subtree file.
    let dir = workspace();
    let home = TempDir::new().unwrap();
    let (addr, _bodies) = endpoint(reads_the_api_crate);

    let first = repl_afi_in(
        &home,
        &repo(&dir),
        Some(addr),
        &["--instructions", "project"],
        "read it\n/quit\n",
    );
    let session = session_of(&first);
    let second = repl_afi_in(
        &home,
        &repo(&dir),
        Some(addr),
        &["--resume", &session, "--instructions", "project"],
        "/instructions\n/quit\n",
    );

    let listing = String::from_utf8_lossy(&second.stdout);
    let root = listing.matches("repo/AGENTS.md").count();
    assert_eq!(
        root, 1,
        "the root file is the startup walk's, listed once: {listing}"
    );
    assert!(
        listing.contains("2 file(s)"),
        "two files, not three: {listing}"
    );
}

#[test]
fn what_a_resume_believes_was_sent_comes_from_the_session_not_the_wire() {
    // The record used to be recovered by scanning the replayed messages for the block
    // marker, which meant any text in the history could claim a rule had been sent -
    // a file the model `cat`s into a tool result can write that marker, and the real
    // rule was then suppressed for the whole session while the listing reported it.
    //
    // Asserted by taking the session's own record away while leaving the block in the
    // messages: reading the wire would still find it there and stay silent, reading
    // the record finds nothing and offers the rule again.
    let dir = workspace();
    let home = TempDir::new().unwrap();
    let (addr, bodies) = endpoint(reads_the_api_crate);

    let first = repl_afi_in(
        &home,
        &repo(&dir),
        Some(addr),
        &["--instructions", "project"],
        "read it\n/quit\n",
    );
    let session = session_of(&first);
    let path = home.path().join("sessions").join(format!("{session}.json"));
    let mut saved: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).expect("the session must exist"))
            .expect("the session must parse");
    assert!(
        saved["instructions"]
            .as_array()
            .is_some_and(|recorded| !recorded.is_empty()),
        "the run has to have recorded what it sent: {saved}"
    );
    let block_still_there = serde_json::to_string(&saved["messages"])
        .expect("the messages must serialize")
        .contains("Contents of ");
    assert!(block_still_there, "the block stays in the replayed history");
    saved["instructions"] = serde_json::json!([]);
    fs::write(&path, saved.to_string()).expect("the session must rewrite");
    bodies.lock().unwrap().clear();

    repl_afi_in(
        &home,
        &repo(&dir),
        Some(addr),
        &["--resume", &session, "--instructions", "project"],
        "read it again\n/quit\n",
    );

    let sent = tool_results(&bodies);
    let carrying = sent.iter().filter(|text| text.contains(API_RULE)).count();
    assert_eq!(
        carrying, 2,
        "with no record of it, the rule is offered again rather than assumed sent: {sent:#?}"
    );
}

#[test]
fn a_resumed_session_reports_the_blocks_it_replays() {
    // A resumed history replays its tool messages verbatim, so the subtree rules an
    // earlier run loaded are in front of the model whatever this run asked for. The
    // listing has to say so - answering "none loaded" about a conversation visibly
    // carrying rules is the reverse of the question `/instructions` exists to answer -
    // and this run must not send them a second time.
    let dir = workspace();
    let home = TempDir::new().unwrap();
    let (addr, bodies) = endpoint(reads_the_api_crate);

    // Run one loads the subtree file and saves the session.
    let first = repl_afi_in(
        &home,
        &repo(&dir),
        Some(addr),
        &["--instructions", "project"],
        "read it\n/quit\n",
    );
    let session = session_of(&first);
    bodies.lock().unwrap().clear();

    // Run two resumes it and asks for no instructions at all.
    let second = repl_afi_in(
        &home,
        &repo(&dir),
        Some(addr),
        &["--resume", &session, "--instructions", "none"],
        "read it again\n/instructions\n/quit\n",
    );

    let listing = String::from_utf8_lossy(&second.stdout);
    assert!(
        listing.contains("carried in from the resumed session"),
        "the replayed block has to be reported: {listing}"
    );
    let carrying = tool_results(&bodies)
        .iter()
        .filter(|text| text.contains(API_RULE))
        .count();
    assert_eq!(
        carrying,
        1,
        "and not sent again on top of the replay: {:#?}",
        tool_results(&bodies)
    );
}
