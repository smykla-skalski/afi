//! What happens to a subtree instruction block when the history that carried it is
//! rewritten.
//!
//! Every one of these rides in a tool result, so `/reset`, `/compress`, and `--resume`
//! each do something different to it: emptied, kept-or-folded, replayed. The rule is
//! that the model's view and what `/instructions` reports must agree afterwards, and
//! that no block is ever sent twice. Split from `instructions_nested` only to stay
//! under the per-file line cap.

use std::net::TcpListener;
use std::sync::Arc;

use tempfile::TempDir;

mod common;

use common::endpoint::{Bodies, LAST, sent_with_roles, serve, tool_call_per_user_turn};
use common::{DEEP_RULE as API_RULE, repl_afi_in, repo, workspace};

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
    let listener = TcpListener::bind("127.0.0.1:0").expect("the fake endpoint must bind");
    let addr = listener.local_addr().expect("an addr");
    let bodies: Bodies = Arc::default();
    let server = serve(listener, &bodies, |seen| {
        tool_call_per_user_turn(
            seen,
            "read_file",
            &serde_json::json!({"path": "crates/api/src/lib.rs"}),
        )
    });

    repl_afi_in(
        &home,
        &repo(&dir),
        Some(addr),
        &["--instructions", "project"],
        "read it\n/reset\nread it again\n/quit\n",
    );
    drop(server);

    // The last request is the second read's, in a history the reset emptied.
    let after = sent_with_roles(&bodies, LAST, &["tool"]);
    assert!(
        after.iter().any(|text| text.contains(API_RULE)),
        "a fresh history was never told the subtree rules again: {after:#?}"
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
    let listener = TcpListener::bind("127.0.0.1:0").expect("the fake endpoint must bind");
    let addr = listener.local_addr().expect("an addr");
    let bodies: Bodies = Arc::default();
    let server = serve(listener, &bodies, |seen| {
        tool_call_per_user_turn(
            seen,
            "read_file",
            &serde_json::json!({"path": "crates/api/src/lib.rs"}),
        )
    });

    repl_afi_in(
        &home,
        &repo(&dir),
        Some(addr),
        &["--instructions", "project"],
        "read it\n/compress\nread it again\n/quit\n",
    );
    drop(server);

    let carrying = sent_with_roles(&bodies, LAST, &["tool", "user"])
        .iter()
        .filter(|text| text.contains(API_RULE))
        .count();
    assert_eq!(
        carrying,
        1,
        "the fold kept the block, so it must not be sent a second time: {:#?}",
        sent_with_roles(&bodies, LAST, &["tool", "user"])
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
    let listener = TcpListener::bind("127.0.0.1:0").expect("the fake endpoint must bind");
    let addr = listener.local_addr().expect("an addr");
    let bodies: Bodies = Arc::default();
    let server = serve(listener, &bodies, |seen| {
        tool_call_per_user_turn(
            seen,
            "read_file",
            &serde_json::json!({"path": "crates/api/src/lib.rs"}),
        )
    });

    // Run one loads the subtree file and saves the session.
    let first = repl_afi_in(
        &home,
        &repo(&dir),
        Some(addr),
        &["--instructions", "project"],
        "read it\n/quit\n",
    );
    let stdout = String::from_utf8_lossy(&first.stdout);
    let session = stdout
        .split("resume with: afi --resume ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("the run must save a session: {stdout}"))
        .to_string();
    bodies.lock().unwrap().clear();

    // Run two resumes it and asks for no instructions at all.
    let second = repl_afi_in(
        &home,
        &repo(&dir),
        Some(addr),
        &["--resume", &session, "--instructions", "none"],
        "read it again\n/instructions\n/quit\n",
    );
    drop(server);

    let listing = String::from_utf8_lossy(&second.stdout);
    assert!(
        listing.contains("carried in from the resumed session"),
        "the replayed block has to be reported: {listing}"
    );
    let carrying = sent_with_roles(&bodies, LAST, &["tool", "user"])
        .iter()
        .filter(|text| text.contains(API_RULE))
        .count();
    assert_eq!(
        carrying,
        1,
        "and not sent again on top of the replay: {:#?}",
        sent_with_roles(&bodies, LAST, &["tool", "user"])
    );
}
