//! What `/compress` puts on the wire, proved against a real process.
//!
//! The manual counterpart to `autocompress`, and it exists because the two folds used
//! to be different code. `/compress` built its own summary request, and that request
//! carried the instruction sentence with the conversation missing - so the model was
//! asked to summarize a history it had never been shown, and whatever it invented
//! replaced the real one. Every test around the fold passed: the summary came back, it
//! was applied, the history got shorter. Only the request body says which conversation
//! was summarized, so that is what this reads.

use std::net::SocketAddr;

use tempfile::TempDir;

mod common;

use common::endpoint::{Bodies, completion_answer, endpoint, text_answer, wants_stream};
use common::{repl_afi_in, repo, workspace};

/// The distinctive turn the fold has to carry into its prompt.
const EARLIER: &str = "count the crates in this workspace";

/// Streaming turns get an answer; the one non-streaming request is the summary.
fn reply(body: &str) -> String {
    if wants_stream(body) {
        text_answer("done")
    } else {
        completion_answer("the earlier turns, summarized")
    }
}

/// Drive a REPL through `input` and hand back what the endpoint was sent.
fn run(addr: SocketAddr, input: &str) -> (String, TempDir) {
    let dir = workspace();
    let home = TempDir::new().unwrap();
    let output = repl_afi_in(&home, &repo(&dir), Some(addr), &[], input);
    (String::from_utf8_lossy(&output.stdout).into_owned(), dir)
}

/// The one non-streaming body, which is the summary request.
fn summary_request(bodies: &Bodies) -> String {
    let recorded = bodies.lock().expect("the lock must hold").clone();
    let asks: Vec<&String> = recorded.iter().filter(|body| !wants_stream(body)).collect();
    assert_eq!(
        asks.len(),
        1,
        "exactly one fold, and it has to have happened: {recorded:#?}"
    );
    asks[0].clone()
}

#[test]
fn the_fold_asks_about_the_conversation_it_is_folding() {
    let (addr, bodies) = endpoint(reply);
    let (stdout, _dir) = run(
        addr,
        &format!("{EARLIER}\nand now something else\n/compress\n/quit\n"),
    );

    // Without this the rest is vacuous - "too few turns" sends no fold at all.
    assert!(
        stdout.contains("Compressed context"),
        "the fold has to have happened: {stdout}"
    );
    let ask = summary_request(&bodies);
    assert!(
        ask.contains("Summarize the following conversation"),
        "the fold must ask for a summary: {ask}"
    );
    assert!(
        ask.contains(EARLIER),
        "the fold must carry the conversation it is summarizing: {ask}"
    );
}

#[test]
fn a_history_too_short_to_fold_sends_no_request_at_all() {
    // The guard belongs to the planner now rather than to the command, and the
    // command measured it differently: it subtracted one for a system message whether
    // or not there was one.
    let (addr, bodies) = endpoint(reply);
    let (stdout, _dir) = run(addr, "/compress\n/quit\n");

    assert!(
        stdout.contains("Nothing to compress"),
        "an empty history has nothing to fold: {stdout}"
    );
    let recorded = bodies.lock().expect("the lock must hold").clone();
    assert!(
        recorded.iter().all(|body| wants_stream(body)),
        "no summary may be requested for a fold that cannot happen: {recorded:#?}"
    );
}
