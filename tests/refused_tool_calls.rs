//! What `usage.refused_tool_calls` and its two halves report, proved against real
//! processes.
//!
//! The counts exist so a caller can tell a run that was asked to write from one
//! that was not, without parsing the transcript. That only holds end to end: the
//! counters are process-wide, so an exact figure means nothing in the unit-test
//! binary, where every refusing test feeds the same numbers. Here each run is its
//! own process and the figures are checkable.
//!
//! Six runs against one endpoint that answers with a `write_file` call. The policy
//! refuses it; the approval gate refuses it, and lands in the other half of the
//! split; a permitted call that ran and failed is refused by nobody. The last three
//! are the ways a call is thrown away before dispatch can rule on it - arguments
//! that will not parse, and a forced-final turn answered with a tool, alone or
//! beside the answer - which used to report a clean zero for a `--read-only` run
//! that was asked to write.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread::{self, JoinHandle};

use serde_json::Value;
use tempfile::TempDir;

use common::{NOT_FOUND, read_request_body, sse_response, summary_of};

/// Usage on the final chunk, so `usage` is a real object rather than the null a
/// silent provider produces. The figures are arbitrary; only the shape matters.
const USAGE: &str = r#"{"prompt_tokens":120,"completion_tokens":8}"#;

/// What the endpoint answers with.
#[derive(Clone, Copy)]
enum Scenario {
    /// A well-formed `write_file` call, then a final answer once its result lands.
    Write,
    /// A `write_file` call whose arguments are truncated, on every request - the
    /// stream afi retries and then gives up on.
    MalformedWrite,
    /// An empty turn, so the turn limit forces a final; the forced-final request is
    /// then answered with `write_file` instead of `final_answer`.
    WriteOnForcedFinal,
    /// The same, but the forced-final reply carries `write_file` *and* the
    /// `final_answer` it was asked for - the likelier shape, since the model is told
    /// to answer and the blocked call is still in the history it is re-reading.
    WriteBesideAnswerOnForcedFinal,
}

/// One streamed `write_file` call, arguments in a single delta. `target` is embedded
/// as a JSON string so no path could break the frame. `truncate` cuts the tail off
/// the argument blob, which is what a cut stream looks like to the parser. `answer`
/// appends the `final_answer` call the model is told to send, so a batch can carry
/// both.
fn tool_call_body(target: &Path, truncate: bool, answer: bool) -> String {
    let mut arguments = serde_json::to_string(&serde_json::json!({
        "path": target.to_string_lossy(),
        "content": "written\n",
    }))
    .expect("the arguments must serialize");
    if truncate {
        arguments.truncate(arguments.len() - 3);
    }
    let mut calls = vec![serde_json::json!({
        "index": 0,
        "id": "call_1",
        "type": "function",
        "function": {"name": "write_file", "arguments": arguments},
    })];
    if answer {
        calls.push(serde_json::json!({
            "index": 1,
            "id": "call_2",
            "type": "function",
            "function": {"name": "final_answer", "arguments": r#"{"answer":"done"}"#},
        }));
    }
    let delta = serde_json::json!({"tool_calls": calls});
    sse_response([
        format!(r#"{{"choices":[{{"delta":{delta}}}]}}"#),
        format!(r#"{{"choices":[{{"delta":{{}},"finish_reason":"tool_calls"}}],"usage":{USAGE}}}"#),
    ])
}

/// A plain text answer, which ends the turn loop.
fn final_body() -> String {
    sse_response([
        r#"{"choices":[{"delta":{"content":"finished"}}]}"#.to_string(),
        format!(r#"{{"choices":[{{"delta":{{}},"finish_reason":"stop"}}],"usage":{USAGE}}}"#),
    ])
}

/// A turn with no text and no tool call, which spends a step against the turn limit.
fn empty_body() -> String {
    sse_response([format!(
        r#"{{"choices":[{{"delta":{{}},"finish_reason":"stop"}}],"usage":{USAGE}}}"#
    )])
}

/// Answer by what the request carries rather than by how many have arrived: afi
/// probes the context window on the side, and a counter would hand the probe the
/// reply meant for the first turn. A body holding a tool result is a later turn, and
/// one advertising only `final_answer` is the forced-final request.
fn reply_for(body: &str, target: &Path, scenario: Scenario) -> String {
    let forced_final = body.contains(r#""name":"final_answer""#);
    match scenario {
        Scenario::Write if body.contains(r#""role":"tool""#) => final_body(),
        Scenario::Write => tool_call_body(target, false, false),
        Scenario::MalformedWrite => tool_call_body(target, true, false),
        Scenario::WriteOnForcedFinal if forced_final => tool_call_body(target, false, false),
        Scenario::WriteBesideAnswerOnForcedFinal if forced_final => {
            tool_call_body(target, false, true)
        }
        Scenario::WriteOnForcedFinal | Scenario::WriteBesideAnswerOnForcedFinal => empty_body(),
    }
}

fn serve(listener: TcpListener, target: &Path, scenario: Scenario) -> JoinHandle<()> {
    let target = target.to_path_buf();
    thread::spawn(move || {
        while let Ok((stream, _)) = listener.accept() {
            answer(stream, &target, scenario);
        }
    })
}

fn answer(mut stream: TcpStream, target: &Path, scenario: Scenario) {
    let mut reader = BufReader::new(stream.try_clone().expect("the socket must clone"));
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    let body = read_request_body(&mut reader);
    let response = if request_line.contains("/chat/completions") {
        reply_for(&body, target, scenario)
    } else {
        NOT_FOUND.to_string()
    };
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// One-shot against the fake endpoint, with the summary on stdout. `env` carries the
/// knobs a scenario needs, so the retry limit and turn limit are pinned by the test
/// rather than inherited from whatever the defaults become.
fn run_afi(home: &TempDir, addr: SocketAddr, extra: &[&str], env: &[(&str, &str)]) -> Output {
    let mut args = vec!["--summary", "json", "-f", "-"];
    args.extend_from_slice(extra);
    let mut child = Command::new(env!("CARGO_BIN_EXE_afi"))
        .args(&args)
        .env_clear()
        .env("AFI_HOME", home.path())
        .env("HOME", home.path())
        .env("AFI_BASE_URL", format!("http://{addr}/v1"))
        .env("AFI_MODEL", "test-model")
        .envs(env.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("afi must start");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(b"write the file\n")
        .expect("the prompt must write");
    child.wait_with_output().expect("afi must exit")
}

/// The three counts, as `(total, by_policy, by_approval)`.
fn refusals(output: &Output) -> (u64, u64, u64) {
    let usage = summary_of(output)["usage"].clone();
    let count = |key: &str| {
        usage
            .get(key)
            .and_then(Value::as_u64)
            .unwrap_or_else(|| panic!("{key} must be reported: {usage}"))
    };
    (
        count("refused_tool_calls"),
        count("refused_by_policy"),
        count("refused_by_approval"),
    )
}

/// The endpoint, the workspace, and the path the model will ask to write.
struct Fixture {
    addr: SocketAddr,
    /// The path the model's call points at. Kept rather than rebuilt, so a test
    /// names it once and cannot assert on a path the model was never asked for.
    target: PathBuf,
    home: TempDir,
    _workspace: TempDir,
    _server: JoinHandle<()>,
}

impl Fixture {
    /// `target` is where the model's call points, which is not always somewhere
    /// writable - the failing-tool case needs a path that cannot be written.
    fn new(target: &str, scenario: Scenario) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a port must bind");
        let addr = listener.local_addr().expect("the port must be readable");
        let workspace = TempDir::new().unwrap();
        let target = workspace.path().join(target);
        // Left to the process rather than joined: the thread parks in accept.
        let server = serve(listener, &target, scenario);
        Self {
            addr,
            target,
            home: TempDir::new().unwrap(),
            _workspace: workspace,
            _server: server,
        }
    }

    fn run(&self, extra: &[&str], env: &[(&str, &str)]) -> Output {
        run_afi(&self.home, self.addr, extra, env)
    }
}

#[test]
fn a_write_the_policy_blocked_is_counted() {
    let fixture = Fixture::new("blocked.txt", Scenario::Write);

    // --yolo is deliberate: with it, approval cannot be what refused the call, so
    // the count can only be the policy's doing.
    let output = fixture.run(&["--yolo", "--read-only"], &[]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(
        refusals(&output),
        (1, 1, 0),
        "a read-only run that was asked to write must say so, on the policy side of \
         the split.\nstderr: {stderr}"
    );
    assert!(!fixture.target.exists());
    assert!(
        stderr.contains("blocked by policy"),
        "the refusal must be the policy's: {stderr}"
    );
}

#[test]
fn a_write_the_approval_gate_denied_lands_in_the_other_half() {
    // No --yolo and no TTY, which denies by default. It is a real refusal and is
    // counted, but it is not evidence the model reached for a forbidden tool - every
    // mutating call in such a run is denied - so it must not land in by_policy,
    // which is what an audit of a restricted run alerts on.
    let fixture = Fixture::new("denied.txt", Scenario::Write);

    let output = fixture.run(&[], &[]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(refusals(&output), (1, 0, 1), "stderr: {stderr}");
    assert!(!fixture.target.exists());
    assert!(
        stderr.contains("denied by user"),
        "the refusal must be the gate's: {stderr}"
    );
}

#[test]
fn a_tool_that_ran_and_failed_is_not_a_refusal() {
    // The case that keeps the counts worth reading. Nothing refused this write; it
    // reached the filesystem and the filesystem said no, because the directory does
    // not exist. Counting that would drown the signal in ordinary noise.
    let fixture = Fixture::new("no-such-dir/unwritable.txt", Scenario::Write);

    let output = fixture.run(&["--yolo"], &[]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("write_file: failed"),
        "the write must have run and failed: {stderr}"
    );
    assert_eq!(
        refusals(&output),
        (0, 0, 0),
        "a failed tool is an error, not a refusal.\nstderr: {stderr}"
    );
    // Present rather than absent, so a caller can tell zero from an afi too old to
    // report the fields at all - `refusals` panics on a missing key.
}

#[test]
fn a_blocked_write_whose_arguments_would_not_parse_is_still_counted() {
    // The gap that made the field a weak guarantee: the argument blobs are parsed
    // before anything dispatches, so a truncated stream skipped the policy gate and
    // a read-only run that was asked to write reported a clean zero. The retry limit
    // is pinned at 0 so the count is exactly one attempt, not one per recovery.
    let fixture = Fixture::new("malformed.txt", Scenario::MalformedWrite);

    let output = fixture.run(
        &["--yolo", "--read-only"],
        &[("AFI_MALFORMED_STREAM_RETRIES", "0")],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("malformed tool call"),
        "the run must have hit the malformed path: {stderr}"
    );
    assert_eq!(
        refusals(&output),
        (1, 1, 0),
        "a blocked call discarded before dispatch is still a call the run was \
         refused.\nstderr: {stderr}"
    );
    assert!(!fixture.target.exists());
}

#[test]
fn a_blocked_write_on_the_forced_final_turn_is_still_counted() {
    // The other discard path. The turn limit forces a final answer, the model
    // answers with a tool instead, and the turn ends without dispatching it - so
    // the last thing a run did used to vanish from the summary.
    let fixture = Fixture::new("forced.txt", Scenario::WriteOnForcedFinal);

    let output = fixture.run(&["--yolo", "--read-only"], &[("AFI_MAX_MODEL_TURNS", "1")]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("FORCED FINAL FAILED"),
        "the run must have hit the forced-final miss: {stderr}"
    );
    assert_eq!(
        refusals(&output),
        (1, 1, 0),
        "a blocked call on the forced-final turn is still a call the run was \
         refused.\nstderr: {stderr}"
    );
    assert!(!fixture.target.exists());
}

#[test]
fn a_blocked_write_beside_the_forced_final_answer_is_counted_too() {
    // The shape that slips past a count placed after the answer scan: the model
    // sends the answer it was told to send *and* the blocked call. The answer is
    // extracted, the rest of the batch is dropped undispatched, and a run that
    // reported the tool-only case as 1 must not report this one as 0 - the
    // difference is whether the model also obeyed, which an audit cannot rely on.
    let fixture = Fixture::new("beside.txt", Scenario::WriteBesideAnswerOnForcedFinal);

    let output = fixture.run(&["--yolo", "--read-only"], &[("AFI_MAX_MODEL_TURNS", "1")]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let summary = summary_of(&output);

    assert_eq!(
        summary["answer"], "done",
        "the forced final answer must still be taken: {stderr}"
    );
    assert_eq!(
        refusals(&output),
        (1, 1, 0),
        "the blocked call riding alongside the answer must still count.\nstderr: {stderr}"
    );
    assert!(!fixture.target.exists());
}
