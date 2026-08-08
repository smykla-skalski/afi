//! A session that crosses the auto-compress threshold folds itself, proved
//! against a real process and a real socket.
//!
//! `AFI_AUTOCOMPRESS_PERCENT` described a fold nothing performed: the function
//! implementing it was reachable only from its own tests, so a long run went on
//! growing until the provider refused it for length. Asking the struct whether it
//! would fold is what let that hold for as long as it did, so this asks the wire
//! instead - every request the run makes is recorded, and the assertions are
//! about what was actually sent.
//!
//! Two runs against one endpoint. The first declares a window small enough for
//! the turn to cross it and expects the fold: a summary request carrying the
//! conversation, a following turn carrying the summary in place of the turns it
//! replaced, and a `requests` count that includes the fold. The second declares
//! nothing, so nothing knows the window, and expects no fold at all - the case
//! the issue asks for by name, and the one that says so on stderr rather than
//! passing in silence.

mod common;

use std::io::Write;
use std::net::{SocketAddr, TcpListener};
use std::process::{Command, Output, Stdio};

use serde_json::Value;
use tempfile::TempDir;

use common::endpoint::{Bodies, serve, sse_body};
use common::summary_of;

/// Prompt tokens the endpoint reports on every turn: 950 of the 1000-token window
/// the folding run declares, which is 95% and over the 85% default threshold.
const PROMPT_TOKENS: u64 = 950;

/// The window that run declares. Small enough to cross with a canned figure, and
/// nothing like a real one - which is the point of it being declared rather than
/// guessed.
const DECLARED_WINDOW: &str = "1000";

fn usage() -> String {
    format!(r#"{{"prompt_tokens":{PROMPT_TOKENS},"completion_tokens":8}}"#)
}

/// A `list_dir` call on the working directory: a tool the model can ask for that
/// changes nothing, so the run reaches a second turn without touching the disk.
fn tool_call_body() -> String {
    let calls = serde_json::json!([{
        "index": 0,
        "id": "call_1",
        "type": "function",
        "function": {"name": "list_dir", "arguments": r#"{"path":"."}"#},
    }]);
    let delta = serde_json::json!({"tool_calls": calls});
    sse_body([
        format!(r#"{{"choices":[{{"delta":{delta}}}]}}"#),
        format!(
            r#"{{"choices":[{{"delta":{{}},"finish_reason":"tool_calls"}}],"usage":{}}}"#,
            usage()
        ),
    ])
}

/// A plain answer, which ends the turn loop.
fn final_body() -> String {
    sse_body([
        r#"{"choices":[{"delta":{"content":"finished"}}]}"#.to_string(),
        format!(
            r#"{{"choices":[{{"delta":{{}},"finish_reason":"stop"}}],"usage":{}}}"#,
            usage()
        ),
    ])
}

/// The non-streaming answer a summary request gets. Carries `usage`, because a
/// request the provider gave no numbers for is deliberately not counted, and the
/// point of this fixture is that the fold *is* counted.
fn summary_body() -> String {
    serde_json::json!({
        "choices": [{"message": {"content": "the earlier turns, summarized"}}],
        "usage": {"prompt_tokens": 40, "completion_tokens": 6},
    })
    .to_string()
}

/// Answer by what the request carries rather than by counting them: the fold puts
/// a second kind of request on the same endpoint, and a counter would hand one of
/// them the reply meant for the other.
///
/// Returns the body only - `common::endpoint::serve` puts the HTTP response
/// around it.
fn reply_for(body: &str) -> String {
    if body.contains(r#""stream":false"#) {
        // The only non-streaming request afi makes is the one asking for a
        // summary.
        return summary_body();
    }
    if body.contains(r#""role":"tool""#) {
        return final_body();
    }
    tool_call_body()
}

/// One-shot against the fake endpoint, with the summary on stdout. `extra` is
/// where a run says what its context window is - or says nothing, which is the
/// other case under test.
fn run_afi(home: &TempDir, addr: SocketAddr, extra: &[&str]) -> Output {
    let mut args = vec!["--yolo", "--summary", "json", "-f", "-"];
    args.extend_from_slice(extra);
    let mut child = Command::new(env!("CARGO_BIN_EXE_afi"))
        .args(&args)
        .env_clear()
        .env("AFI_HOME", home.path())
        .env("HOME", home.path())
        .env("AFI_BASE_URL", format!("http://{addr}/v1"))
        .env("AFI_MODEL", "a-model-no-table-knows")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("afi must start");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(b"look around and report\n")
        .expect("the prompt must write");
    child.wait_with_output().expect("afi must exit")
}

fn endpoint() -> (SocketAddr, Bodies) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port must bind");
    let addr = listener.local_addr().expect("the address must resolve");
    let bodies: Bodies = Bodies::default();
    let _server = serve(listener, &bodies, reply_for);
    (addr, bodies)
}

/// Every `/chat/completions` body the run sent, in order.
fn sent(bodies: &Bodies) -> Vec<String> {
    bodies.lock().expect("the lock must hold").clone()
}

/// Whether `body` is a turn rather than the fold: the streaming requests are the
/// conversation, the one non-streaming request is the summary.
fn is_turn(body: &str) -> bool {
    body.contains(r#""stream":true"#)
}

/// Assert that the fold asked for a summary and carried the turns it was
/// summarizing. A request that went out empty would be a fold that destroyed
/// context rather than condensing it.
fn assert_asks_for_a_summary(fold: &str) {
    assert!(
        fold.contains("Summarize the following conversation history"),
        "the fold must ask for a summary: {fold}"
    );
    assert!(
        fold.contains("look around and report"),
        "the fold must carry the conversation it is summarizing: {fold}"
    );
}

/// Assert that the fold reached the wire: the turn after it sends the summary in
/// place of the turns it replaced.
fn assert_carries_the_summary(after: &str) {
    assert!(
        after.contains("[Compressed context"),
        "the turn after the fold must carry the summary: {after}"
    );
    assert!(
        after.contains("the earlier turns, summarized"),
        "the summary the model wrote is what the fold keeps: {after}"
    );
}

#[test]
fn a_run_over_the_threshold_folds_its_context_and_counts_the_request() {
    let (addr, bodies) = endpoint();
    let home = TempDir::new().expect("a temp home must exist");

    let output = run_afi(&home, addr, &["--context-window", DECLARED_WINDOW]);
    let stderr = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);
    let summary = summary_of(&output);
    let requests = sent(&bodies);

    // The request that produces the summary carries the turns being summarized.
    // Asking only that a request went out would pass for a fold that summarized
    // nothing, which is its own open issue.
    let fold = requests
        .iter()
        .find(|body| !is_turn(body))
        .unwrap_or_else(|| {
            panic!("the run must have asked for a summary; it sent {requests:#?}\n{stderr}")
        });
    assert_asks_for_a_summary(fold);

    let after = requests
        .iter()
        .rfind(|body| is_turn(body))
        .expect("the run must have taken another turn");
    assert_carries_the_summary(after);

    // The compression request is billed like any other, so it is counted like any
    // other - the same rule `/compress` already follows.
    let folds = requests.len() - requests.iter().filter(|body| is_turn(body)).count();
    assert_eq!(folds, 1, "one fold, not one per turn: {requests:#?}");
    assert_eq!(
        summary["usage"]["requests"],
        Value::from(u64::try_from(requests.len()).expect("a small count")),
        "every request the run made must be counted: {summary}"
    );
    assert_eq!(summary["ok"], true, "{summary}\n{stderr}");
}

#[test]
fn a_run_with_no_known_context_window_does_not_fold() {
    let (addr, bodies) = endpoint();
    let home = TempDir::new().expect("a temp home must exist");

    // No `--context-window`, and a model name no compiled row answers for, so the
    // threshold has nothing to measure against.
    let output = run_afi(&home, addr, &[]);
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let requests = sent(&bodies);

    assert!(
        requests
            .iter()
            .all(|body| body.contains(r#""stream":true"#)),
        "a run with no window must not ask for a summary: {requests:#?}"
    );
    // Silence would read as health here: the setting is on, the run is over what
    // would have been the threshold, and nothing happened.
    assert!(
        stderr.contains("context window") && stderr.contains("CONTEXT_WINDOW"),
        "the run must say why it is not compressing, and name the setting: {stderr}"
    );
}
