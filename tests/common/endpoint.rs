//! A canned OpenAI-compatible endpoint, for tests that need to read what a run
//! actually put on the wire rather than ask a struct what it would have.
//!
//! Every request body is recorded, so a test can assert on what was sent as well
//! as on what the run did with the answer.

// Compiled into every test binary that pulls in `common`, most of which want
// none of this.
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

/// Every `/chat/completions` body the endpoint was sent.
pub type Bodies = Arc<Mutex<Vec<String>>>;

/// `events` framed as an SSE body, terminated the way a provider ends one.
///
/// The body only - [`serve`] puts the HTTP response around it, which is what
/// separates this from `common::sse_response`. One place owns the frame so a test
/// that needs its own events does not hand-roll `data:` lines to get them.
pub fn sse_body<I>(events: I) -> String
where
    I: IntoIterator,
    I::Item: AsRef<str>,
{
    let mut body = String::new();
    for event in events {
        body.push_str("data: ");
        body.push_str(event.as_ref());
        body.push_str("\n\n");
    }
    body.push_str("data: [DONE]\n\n");
    body
}

/// One plain text answer, which ends the turn loop after a single request.
pub fn text_answer(text: &str) -> String {
    let content = serde_json::json!({"content": text});
    sse_body([
        format!(r#"{{"choices":[{{"delta":{content}}}]}}"#),
        r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#.to_string(),
    ])
}

/// One structured tool call, which the turn loop dispatches before asking again.
pub fn tool_call_answer(name: &str, args: &serde_json::Value) -> String {
    let arguments = serde_json::to_string(args).expect("the arguments must serialize");
    let call = serde_json::json!({"tool_calls": [{
        "index": 0, "id": "call_1", "type": "function",
        "function": {"name": name, "arguments": arguments},
    }]});
    [
        format!(r#"data: {{"choices":[{{"delta":{call}}}]}}"#),
        r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#.to_string(),
        "data: [DONE]".to_string(),
    ]
    .join("\n\n")
        + "\n\n"
}

/// The same call until the history carries `calls` results of it, then an answer.
///
/// Keyed on what the request carries rather than on a counter, for the reason
/// [`serve`] documents: the context-window probe would otherwise be handed the
/// answer meant for a turn.
pub fn tool_call_then_text(
    body: &str,
    name: &str,
    args: &serde_json::Value,
    calls: usize,
) -> String {
    if body.matches(r#""role":"tool""#).count() >= calls {
        return text_answer("finished");
    }
    tool_call_answer(name, args)
}

/// One non-streaming chat completion, as a JSON body rather than an SSE stream.
///
/// `/compress` asks for its summary through the non-streaming path and parses
/// `choices[0].message.content`, so a server that answers everything as SSE makes the
/// fold silently no-op - which is exactly how a test spanning `/compress` ends up
/// asserting nothing.
pub fn completion_answer(text: &str) -> String {
    serde_json::json!({"choices": [{"message": {"content": text}}]}).to_string()
}

/// Whether a recorded request asked for a stream. The non-streaming ones are the
/// compression summary and the context-window probe.
pub fn wants_stream(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .is_some_and(|parsed| parsed["stream"] == true)
}

/// A tool call whenever the request ends on a user turn, an answer otherwise.
///
/// Keyed on the last message rather than on a count of tool results, which is what a
/// test spanning `/compress` needs: the fold keeps recent turns, so a counter sees
/// the surviving result and stops asking - and the second read the test is about
/// never happens.
pub fn tool_call_per_user_turn(body: &str, name: &str, args: &serde_json::Value) -> String {
    if !wants_stream(body) {
        return completion_answer("earlier turns, summarized");
    }
    let parsed: serde_json::Value = serde_json::from_str(body).expect("the body must parse");
    let last_is_user = parsed["messages"]
        .as_array()
        .and_then(|messages| messages.last())
        .is_some_and(|message| message["role"] == "user");
    if last_is_user {
        return tool_call_answer(name, args);
    }
    text_answer("finished")
}

/// The system message of a recorded request, asserting there is exactly one.
///
/// Beside `Bodies` because every test that asks "what landed in the system content"
/// needs the same three steps - take a body, parse it, filter the roles - and the
/// assertion is half the value: the Anthropic path joins multiple system messages
/// into one block, so a second one is a bug that hides itself.
///
/// `nth` is which recorded request to read. `0` is the first, which is what a
/// one-turn run sends; a multi-turn run wants the last, since each turn resends the
/// whole history and folding them together counts every message once per turn that
/// followed it.
pub fn system_sent(bodies: &Bodies, nth: usize) -> String {
    let messages = messages_of(bodies, nth);
    let system: Vec<&str> = messages
        .iter()
        .filter(|message| message["role"] == "system")
        .filter_map(|message| message["content"].as_str())
        .collect();
    assert_eq!(system.len(), 1, "exactly one system message: {messages:?}");
    system[0].to_string()
}

/// The contents of every message with one of `roles`, in order.
pub fn sent_with_roles(bodies: &Bodies, nth: usize, roles: &[&str]) -> Vec<String> {
    messages_of(bodies, nth)
        .iter()
        .filter(|message| roles.iter().any(|role| message["role"] == *role))
        .filter_map(|message| message["content"].as_str().map(str::to_string))
        .collect()
}

/// The messages of one recorded request, parsed.
fn messages_of(bodies: &Bodies, nth: usize) -> Vec<serde_json::Value> {
    let bodies = bodies.lock().expect("the lock must hold");
    let body = match nth {
        usize::MAX => bodies.last(),
        n => bodies.get(n),
    }
    .expect("a request must have been sent");
    let parsed: serde_json::Value =
        serde_json::from_str(body).expect("the request body must parse");
    parsed["messages"]
        .as_array()
        .expect("a request carries messages")
        .clone()
}

/// Read the first recorded request, which is the only one a single-turn run sends.
pub const FIRST: usize = 0;

/// Read the last recorded request rather than a numbered one.
pub const LAST: usize = usize::MAX;

/// A bound endpoint answering with `reply`, and the bodies it records.
///
/// The join handle stays here rather than going back to the caller, because dropping
/// one does not stop a thread: it detaches it. Every caller that took the handle only
/// to `drop` it at the end of the test was writing a line that did nothing, and the
/// line read like a shutdown. The thread parks on `accept` until the test binary exits,
/// which is what these tests want - the runs are sequential and each reads back the
/// body it alone sent.
pub fn endpoint<R>(reply: R) -> (SocketAddr, Bodies)
where
    R: Fn(&str) -> String + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").expect("the fake endpoint must bind");
    let addr = listener
        .local_addr()
        .expect("the endpoint must have an addr");
    let bodies: Bodies = Arc::default();
    drop(serve(listener, &bodies, reply));
    (addr, bodies)
}

/// Answers every user turn with a `read_file` on the api crate's source, which is what
/// the subtree-loading tests drive.
pub fn reads_the_api_crate(body: &str) -> String {
    tool_call_per_user_turn(
        body,
        "read_file",
        &serde_json::json!({"path": "crates/api/src/lib.rs"}),
    )
}

/// The contents of every tool result and user message in the last recorded request.
pub fn tool_results(bodies: &Bodies) -> Vec<String> {
    sent_with_roles(bodies, LAST, &["tool", "user"])
}

/// Answer `/chat/completions` with `reply(body)`, recording every body first.
///
/// `reply` takes the request body rather than a counter: afi probes the context
/// window on the side, and counting requests hands the probe the answer meant
/// for the first turn.
fn serve<R>(listener: TcpListener, bodies: &Bodies, reply: R) -> JoinHandle<()>
where
    R: Fn(&str) -> String + Send + 'static,
{
    let bodies = Arc::clone(bodies);
    thread::spawn(move || {
        while let Ok((stream, _)) = listener.accept() {
            answer(stream, &bodies, &reply);
        }
    })
}

fn answer<R: Fn(&str) -> String>(mut stream: TcpStream, bodies: &Bodies, reply: &R) {
    let mut reader = BufReader::new(stream.try_clone().expect("the socket must clone"));
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    let body = super::read_request_body(&mut reader);
    let response = if request_line.contains("/chat/completions") {
        bodies
            .lock()
            .expect("the lock must hold")
            .push(body.clone());
        let answer = reply(&body);
        let kind = if wants_stream(&body) {
            "text/event-stream"
        } else {
            "application/json"
        };
        super::http_response("200 OK", kind, &answer)
    } else {
        // The context-window probe, which `NOT_FOUND` documents afi falls back from.
        super::NOT_FOUND.to_string()
    };
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}
