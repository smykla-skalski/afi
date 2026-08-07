//! What `--system-prompt-file` actually puts on the wire, proved against a real
//! process and a real endpoint.
//!
//! Asking `Runtime` would prove only that the resolver agrees with itself. The
//! claim worth testing is about the request body: that a supplied prompt arrives
//! as system content rather than as a user message, that replacing it drops the
//! shell guidance and keeps the text-protocol contract, and that a run
//! configuring nothing sends the bytes it always has.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use serde_json::Value;
use tempfile::TempDir;

/// Every `/chat/completions` body the endpoint was sent.
type Bodies = Arc<Mutex<Vec<String>>>;

/// A plain text answer, which ends the turn loop after one request.
fn final_body() -> String {
    [
        r#"data: {"choices":[{"delta":{"content":"finished"}}]}"#,
        r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
        "data: [DONE]",
    ]
    .join("\n\n")
        + "\n\n"
}

fn serve(listener: TcpListener, bodies: &Bodies) -> JoinHandle<()> {
    let bodies = Arc::clone(bodies);
    thread::spawn(move || {
        while let Ok((stream, _)) = listener.accept() {
            answer(stream, &bodies);
        }
    })
}

fn answer(mut stream: TcpStream, bodies: &Bodies) {
    let mut reader = BufReader::new(stream.try_clone().expect("the socket must clone"));
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    let body = read_body(&mut reader);
    let response = if request_line.contains("/chat/completions") {
        bodies.lock().expect("the lock must hold").push(body);
        let sse = final_body();
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{sse}",
            sse.len()
        )
    } else {
        // The context-window probe. 404 is a fine answer; afi falls back.
        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
    };
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// Read past the headers and whatever body they announce, so the client is not
/// answered before it has finished sending.
fn read_body(reader: &mut BufReader<TcpStream>) -> String {
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
    String::from_utf8_lossy(&body).into_owned()
}

/// One-shot, against `addr` when there is one and a closed port when there is
/// not - a run that refuses to start never reaches either.
fn run_afi(home: &TempDir, addr: Option<SocketAddr>, extra: &[&str]) -> Output {
    let base = addr.map_or_else(
        || "http://127.0.0.1:9/v1".to_string(),
        |addr| format!("http://{addr}/v1"),
    );
    let mut args = vec!["-f", "-"];
    args.extend_from_slice(extra);
    let mut child = Command::new(env!("CARGO_BIN_EXE_afi"))
        .args(&args)
        .env_clear()
        .env("AFI_HOME", home.path())
        .env("HOME", home.path())
        .env("AFI_BASE_URL", base)
        .env("AFI_MODEL", "test-model")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("afi must start");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(b"review the diff\n")
        .expect("the prompt must write");
    child.wait_with_output().expect("afi must exit")
}

/// The system message of the first request, which is the only one these runs
/// make.
fn system_sent(bodies: &Bodies) -> String {
    let bodies = bodies.lock().expect("the lock must hold");
    let body = bodies.first().expect("a request must have been sent");
    let parsed: Value = serde_json::from_str(body).expect("the request body must parse");
    let messages = parsed["messages"]
        .as_array()
        .expect("a request carries messages");
    let system: Vec<&str> = messages
        .iter()
        .filter(|message| message["role"] == "system")
        .filter_map(|message| message["content"].as_str())
        .collect();
    assert_eq!(system.len(), 1, "exactly one system message: {messages:?}");
    system[0].to_string()
}

/// The user messages of the first request, to prove the supplied prompt did not
/// arrive as one.
fn user_sent(bodies: &Bodies) -> Vec<String> {
    let bodies = bodies.lock().expect("the lock must hold");
    let parsed: Value =
        serde_json::from_str(bodies.first().expect("a request")).expect("it must parse");
    parsed["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .filter(|message| message["role"] == "user")
        .filter_map(|message| message["content"].as_str())
        .map(str::to_string)
        .collect()
}

fn write_prompt(home: &TempDir, body: &str) -> String {
    let path = home.path().join("review.md");
    fs::write(&path, body).expect("the prompt must write");
    path.to_string_lossy().into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("stderr must be utf-8")
}

const SUPPLIED: &str = "You review diffs and nothing else. Never write a file.";
const SHELL_GUIDANCE: &str = "Operating principles for long background commands";
const BUILT_IN_OPENING: &str = "You are a terminal coding agent";
const CONTRACT: &str = "[afi_tool_call]";

/// One server for the three runs that need one. Sequential rather than three
/// listeners, since each run has to read back the body it alone sent.
#[test]
fn the_prompt_a_run_configures_is_what_it_sends() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("the fake endpoint must bind");
    let addr = listener
        .local_addr()
        .expect("the endpoint must have an addr");
    let bodies: Bodies = Arc::default();
    let server = serve(listener, &bodies);

    nothing_configured_sends_the_built_in_prompt(addr, &bodies);
    replace_drops_the_guidance_and_keeps_the_contract(addr, &bodies);
    append_sends_both(addr, &bodies);

    drop(server);
}

fn nothing_configured_sends_the_built_in_prompt(addr: SocketAddr, bodies: &Bodies) {
    let home = TempDir::new().unwrap();
    run_afi(&home, Some(addr), &[]);

    let system = system_sent(bodies);
    assert!(system.starts_with(BUILT_IN_OPENING), "{system}");
    assert!(system.contains(SHELL_GUIDANCE), "{system}");
    assert!(system.contains(CONTRACT), "{system}");
    bodies.lock().unwrap().clear();
}

fn replace_drops_the_guidance_and_keeps_the_contract(addr: SocketAddr, bodies: &Bodies) {
    let home = TempDir::new().unwrap();
    let path = write_prompt(&home, SUPPLIED);
    run_afi(&home, Some(addr), &["--system-prompt-file", &path]);

    let system = system_sent(bodies);
    assert!(
        system.contains(SUPPLIED),
        "the supplied text is sent: {system}"
    );
    assert!(
        !system.contains(SHELL_GUIDANCE),
        "replace drops the shell guidance, which is the point: {system}"
    );
    assert!(
        !system.contains(BUILT_IN_OPENING),
        "a replaced prompt says who the agent is itself: {system}"
    );
    assert!(
        system.contains(CONTRACT),
        "the wire contract survives, or a model on an endpoint with no native \
         tool calls cannot call one: {system}"
    );
    assert_eq!(
        user_sent(bodies),
        vec!["review the diff".to_string()],
        "the supplied prompt is system content, not a second user message"
    );
    bodies.lock().unwrap().clear();
}

fn append_sends_both(addr: SocketAddr, bodies: &Bodies) {
    let home = TempDir::new().unwrap();
    let path = write_prompt(&home, SUPPLIED);
    run_afi(
        &home,
        Some(addr),
        &[
            "--system-prompt-file",
            &path,
            "--system-prompt-mode",
            "append",
        ],
    );

    let system = system_sent(bodies);
    assert!(system.starts_with(BUILT_IN_OPENING), "{system}");
    assert!(system.contains(SHELL_GUIDANCE), "{system}");
    assert!(
        system.ends_with(SUPPLIED),
        "the supplied text lands last: {system}"
    );
    bodies.lock().unwrap().clear();
}

#[test]
fn the_summary_names_the_prompt_the_run_used() {
    // So a CI job's behaviour can be read out of its own output rather than out
    // of the workflow file that was supposed to have configured it.
    let home = TempDir::new().unwrap();
    let path = write_prompt(&home, SUPPLIED);
    let summary = home.path().join("run.json");
    run_afi(
        &home,
        None,
        &[
            "--system-prompt-file",
            &path,
            "--summary-file",
            summary.to_str().unwrap(),
        ],
    );

    let reported = read_json(&summary);
    assert_eq!(reported["system_prompt"]["mode"], "replace");
    assert_eq!(reported["system_prompt"]["file"], path);
}

#[test]
fn an_unconfigured_run_reports_the_built_in_prompt() {
    let home = TempDir::new().unwrap();
    let summary = home.path().join("run.json");
    run_afi(&home, None, &["--summary-file", summary.to_str().unwrap()]);

    let reported = read_json(&summary);
    assert_eq!(reported["system_prompt"]["mode"], "builtin");
    assert_eq!(reported["system_prompt"]["file"], Value::Null);
}

fn read_json(path: &Path) -> Value {
    let body = fs::read_to_string(path).expect("the summary file must exist");
    serde_json::from_str(&body).expect("the summary must parse whole")
}

// --- refusals -----------------------------------------------------------------

/// Every one of these has to exit 2 and name what it could not use. The
/// alternative in each case is a run that sends afi's own prompt while its
/// command line says it is sending something else.
#[test]
fn a_prompt_that_cannot_be_used_refuses_the_run() {
    let home = TempDir::new().unwrap();
    let missing = home.path().join("absent.md");
    let missing = missing.to_str().unwrap();
    let empty = write_prompt(&home, "   \n\n");

    for (label, args, expected) in [
        (
            "a missing file",
            vec!["--system-prompt-file", missing],
            missing,
        ),
        (
            "an empty file",
            vec!["--system-prompt-file", &empty],
            empty.as_str(),
        ),
        (
            "an unknown mode",
            vec![
                "--system-prompt-file",
                &empty,
                "--system-prompt-mode",
                "repalce",
            ],
            "repalce",
        ),
        (
            "no value at all",
            vec!["--system-prompt-file"],
            "--system-prompt-file",
        ),
        (
            "a value that is another flag",
            vec!["--system-prompt-file", "--yolo"],
            "--system-prompt-file",
        ),
        (
            "an unset shell variable, quoted",
            vec!["--system-prompt-file", ""],
            "--system-prompt-file",
        ),
        (
            "a mode with no value",
            vec!["--system-prompt-mode"],
            "--system-prompt-mode",
        ),
    ] {
        let output = run_afi(&home, None, &args);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{label} must refuse the run, stderr: {}",
            stderr_of(&output)
        );
        let stderr = stderr_of(&output);
        assert!(
            stderr.contains(expected),
            "{label} must name {expected:?}, said: {stderr}"
        );
    }
}

#[test]
fn a_blank_variable_is_not_a_refusal() {
    // An exported-but-unset variable is how a workflow leaves the setting off
    // for a job, so it has to mean "no prompt file" rather than "stop".
    let home = TempDir::new().unwrap();
    let summary = home.path().join("run.json");
    let mut child = Command::new(env!("CARGO_BIN_EXE_afi"))
        .args(["-f", "-", "--summary-file", summary.to_str().unwrap()])
        .env_clear()
        .env("AFI_HOME", home.path())
        .env("HOME", home.path())
        .env("AFI_BASE_URL", "http://127.0.0.1:9/v1")
        .env("AFI_MODEL", "test-model")
        .env("AFI_SYSTEM_PROMPT_FILE", "")
        .env("AFI_SYSTEM_PROMPT_MODE", "")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("afi must start");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(b"hello\n")
        .expect("the prompt must write");
    let output = child.wait_with_output().expect("afi must exit");

    assert_ne!(output.status.code(), Some(2), "{}", stderr_of(&output));
    assert_eq!(read_json(&summary)["system_prompt"]["mode"], "builtin");
}
