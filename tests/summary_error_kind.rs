//! End-to-end: what a real `afi` process puts in `error_kind` for each way a run
//! can die.
//!
//! The mapping is unit tested on its own. What a unit test cannot see is whether
//! the kind survives the whole path - a status off the socket, through the client,
//! the turn loop, and the summary assembly - so this drives the binary against a
//! server that answers exactly one way and reads the JSON off stdout.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Command, Output, Stdio};
use std::thread::{self, JoinHandle};

use serde_json::Value;
use tempfile::TempDir;

/// Answer every connection with `response` until the listener closes. The thread
/// is left to the process rather than joined: it parks in `accept`.
fn serve(listener: TcpListener, response: String) -> JoinHandle<()> {
    thread::spawn(move || {
        while let Ok((stream, _)) = listener.accept() {
            answer(stream, &response);
        }
    })
}

fn answer(mut stream: TcpStream, response: &str) {
    let mut reader = BufReader::new(stream.try_clone().expect("the socket must clone"));
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    drain_body(&mut reader);
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// Read past the headers and whatever body they announce, so the client is not
/// answered before it has finished sending.
fn drain_body(reader: &mut BufReader<TcpStream>) {
    let mut length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; length];
    let _ = reader.read_exact(&mut body);
}

fn http_response(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// A turn that only ever calls a tool, so a forced final gets a tool call where
/// an answer belongs.
fn tool_call_body() -> String {
    sse([
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","type":"function","function":{"name":"list_dir","arguments":"{\"path\":\".\"}"}}]}}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
    ])
}

/// A turn that answers with an empty `final_answer`.
fn empty_answer_body() -> String {
    sse([
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","type":"function","function":{"name":"final_answer","arguments":"{\"answer\":\"\"}"}}]}}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
    ])
}

/// A turn whose tool-call arguments are not JSON.
fn malformed_args_body() -> String {
    sse([
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","type":"function","function":{"name":"list_dir","arguments":"{\"path\": "}}]}}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
    ])
}

/// A turn that streams nothing at all.
fn empty_body() -> String {
    sse([
        r#"{"choices":[{"delta":{"content":""}}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
    ])
}

fn sse<const N: usize>(events: [&str; N]) -> String {
    let mut body = String::new();
    for event in events {
        body.push_str("data: ");
        body.push_str(event);
        body.push_str("\n\n");
    }
    body.push_str("data: [DONE]\n\n");
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// Run the binary against `base_url`, one prompt on stdin, summary on stdout.
fn run_afi(home: &TempDir, base_url: &str, args: &[&str]) -> Output {
    run_afi_with(home, base_url, args, &[])
}

fn run_afi_with(home: &TempDir, base_url: &str, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_afi"));
    command
        .args(["--yolo", "--summary", "json"])
        .args(args)
        .env_clear()
        .env("AFI_HOME", home.path())
        .env("HOME", home.path())
        .env("AFI_BASE_URL", base_url)
        .env("AFI_MODEL", "test-model")
        .envs(env.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("afi must start");
    // A run that refuses to start is gone before this lands, and that is the
    // behaviour under test rather than a failure.
    let _ = child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(b"say done\n");
    child.wait_with_output().expect("afi must exit")
}

fn summary_of(output: &Output) -> Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout
        .lines()
        .find(|line| line.trim_start().starts_with('{'))
        .unwrap_or_else(|| panic!("no JSON summary on stdout: {stdout}"));
    serde_json::from_str(line).expect("the summary must be JSON")
}

/// Drive one canned HTTP response through a real run and return its summary.
fn summary_for(response: String) -> Value {
    summary_for_with(response, &[])
}

/// The same, with extra environment - `AFI_MAX_MODEL_TURNS=1` to reach the forced
/// final in one step rather than two hundred.
fn summary_for_with(response: String, env: &[(&str, &str)]) -> Value {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port must bind");
    let addr: SocketAddr = listener.local_addr().expect("the port must be readable");
    let _server = serve(listener, response);
    let home = TempDir::new().unwrap();
    let output = run_afi_with(&home, &format!("http://{addr}/v1"), &["-f", "-"], env);
    assert_eq!(
        output.status.code(),
        Some(1),
        "a failed run must still exit 1: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    summary_of(&output)
}

#[test]
fn a_rejected_credential_is_reported_as_auth() {
    // The failure a caller must never retry. Substring-matching the sentence is
    // what this field replaces, so the assertion is on the field alone.
    let summary = summary_for(http_response(
        "401 Unauthorized",
        r#"{"error":{"type":"authentication_error"}}"#,
    ));
    assert_eq!(summary["ok"], false);
    assert_eq!(summary["error_kind"], "auth");
    // The kind is what a workflow branches on; the sentence is what it can report
    // without anyone opening the log.
    let error = summary["error"].as_str().unwrap_or_default();
    assert!(error.contains("HTTP 401"), "{error}");
    assert!(error.contains("authentication_error"), "{error}");
}

#[test]
fn a_rate_limit_is_reported_as_the_providers_own_trouble() {
    // The same shape of failure as the 401 on the wire, and the opposite decision:
    // this one is worth another attempt.
    let summary = summary_for(http_response(
        "429 Too Many Requests",
        r#"{"error":{"type":"rate_limit_error"}}"#,
    ));
    assert_eq!(summary["error_kind"], "provider_http");
}

#[test]
fn a_gateway_timeout_is_reported_as_a_timeout() {
    let summary = summary_for(http_response("504 Gateway Timeout", "upstream gone"));
    assert_eq!(summary["error_kind"], "timeout");
}

#[test]
fn a_success_that_is_not_a_stream_is_reported_against_the_stream() {
    // A proxy answering 200 with a JSON error body. The request was accepted, so
    // the fault is in what came back rather than in reaching the server.
    let summary = summary_for(http_response("200 OK", r#"{"error":"proxy failure"}"#));
    assert_eq!(summary["error_kind"], "provider_stream");
}

#[test]
fn an_unreadable_prompt_file_is_reported_as_input() {
    // Nothing was sent, so nothing about the provider is knowable. Reporting this
    // as a provider failure would send a retry at a server that never saw it.
    let home = TempDir::new().unwrap();
    let missing = home.path().join("no-such-prompt.txt");
    let output = run_afi(
        &home,
        "http://127.0.0.1:9/v1",
        &["-f", missing.to_str().unwrap()],
    );
    let summary = summary_of(&output);
    assert_eq!(summary["error_kind"], "input");
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn a_policy_that_cannot_be_honoured_is_reported_as_policy() {
    // The one failure that keeps its own exit code. A caller parsing stdout used
    // to get an empty pipe here and had to read stderr to find out why.
    let home = TempDir::new().unwrap();
    let output = run_afi(
        &home,
        "http://127.0.0.1:9/v1",
        &["--disallowed-tools", "run_bsah", "-f", "-"],
    );
    assert_eq!(output.status.code(), Some(2), "the exit code is unchanged");
    let summary = summary_of(&output);
    assert_eq!(summary["ok"], false);
    assert_eq!(summary["error_kind"], "policy");
    assert!(
        summary["error"]
            .as_str()
            .unwrap_or_default()
            .contains("run_bsah"),
        "the reason must name the typo: {summary}"
    );
    // Nothing ran, so nothing may be reported as having been permitted to.
    assert_eq!(summary["tools"], serde_json::json!([]));
}

#[test]
fn an_unreachable_server_is_reported_against_the_provider() {
    // Port 9 discards, so nothing answers. The run never got a status, which is
    // still the provider's end of the connection.
    let home = TempDir::new().unwrap();
    let output = run_afi(&home, "http://127.0.0.1:9/v1", &["-f", "-"]);
    let summary = summary_of(&output);
    assert_eq!(summary["error_kind"], "provider_http");
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn a_forced_final_that_answers_with_a_tool_call_reports_no_answer() {
    // The model never says anything: every turn is a tool call, so the forced final
    // gets one too and afi gives up. The run has no answer, and it used to report
    // ok:true and exit 0 anyway.
    let summary = summary_for_with(tool_call_body(), &[("AFI_MAX_MODEL_TURNS", "1")]);
    assert_eq!(summary["ok"], false);
    assert_eq!(summary["error_kind"], "no_answer");
    let error = summary["error"].as_str().unwrap_or_default();
    assert!(error.contains("FORCED FINAL FAILED"), "{error}");
    assert!(error.contains("list_dir"), "{error}");
    // The provider was reached and billed, so the counts stand next to the failure.
    assert!(
        summary["usage"]["requests"].as_u64().unwrap_or(0) >= 2,
        "{summary}"
    );
}

#[test]
fn a_run_that_streams_nothing_reports_no_answer() {
    // Empty turn, nudge, forced final, empty again. Nothing was ever said.
    let summary = summary_for_with(empty_body(), &[("AFI_MAX_MODEL_TURNS", "1")]);
    assert_eq!(summary["ok"], false);
    assert_eq!(summary["error_kind"], "no_answer");
    assert_eq!(summary["answer"], "");
    assert!(
        summary["error"]
            .as_str()
            .unwrap_or_default()
            .contains("NO ANSWER"),
        "{summary}"
    );
}

#[test]
fn an_empty_forced_final_answer_reports_no_answer() {
    // The model called final_answer with nothing in it. Reported DONE, so the run
    // came back ok:true with an empty answer for CI to post.
    let summary = summary_for_with(empty_answer_body(), &[("AFI_MAX_MODEL_TURNS", "1")]);
    assert_eq!(summary["ok"], false);
    assert_eq!(summary["error_kind"], "no_answer");
    assert!(
        summary["error"]
            .as_str()
            .unwrap_or_default()
            .contains("FORCED FINAL ANSWER EMPTY"),
        "{summary}"
    );
}

#[test]
fn giving_up_on_malformed_tool_arguments_reports_no_answer() {
    // Arguments that are not JSON, with no recoveries allowed. Nothing was
    // dispatched and nothing was said, so the turn produced no answer.
    let summary = summary_for_with(
        malformed_args_body(),
        &[("AFI_MALFORMED_STREAM_RETRIES", "0")],
    );
    assert_eq!(summary["ok"], false);
    assert_eq!(summary["error_kind"], "no_answer");
    assert!(
        summary["error"]
            .as_str()
            .unwrap_or_default()
            .contains("malformed tool call"),
        "{summary}"
    );
}
