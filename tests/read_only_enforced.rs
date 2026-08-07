//! Whether `--read-only` actually stops a write, proved against a real process.
//!
//! This exists because the posture shipped enforcing nothing, and every test
//! that was supposed to catch it passed. They all asked `Runtime::tool_policy`,
//! which feeds the banner and the run summary; the dispatcher reads
//! `ModelConfig::tool_policy`, which was built without the read-only input. The
//! summary printed `["read_file", "list_dir"]` while the same run wrote a file.
//!
//! So this asks nothing. It runs the binary against an endpoint that answers with
//! a `write_file` call and looks at the filesystem. The control run - identical
//! but for the flag - has to write the file, or "no file" would prove only that
//! something else broke.

mod common;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use serde_json::Value;
use tempfile::TempDir;

use common::{NOT_FOUND, read_request_body, sse_response};

/// Every `/chat/completions` body the endpoint was sent, so the test can check
/// what the run advertised as well as what it did.
type Bodies = Arc<Mutex<Vec<String>>>;

/// One streamed `write_file` call, arguments in a single delta. `target` is
/// embedded as a JSON string so a Windows-style path could not break the frame.
fn tool_call_body(target: &Path) -> String {
    let arguments = serde_json::to_string(&serde_json::json!({
        "path": target.to_string_lossy(),
        "content": "written\n",
    }))
    .expect("the arguments must serialize");
    let delta = serde_json::json!({
        "tool_calls": [{
            "index": 0,
            "id": "call_1",
            "type": "function",
            "function": {"name": "write_file", "arguments": arguments},
        }]
    });
    sse_response([
        format!(r#"{{"choices":[{{"delta":{delta}}}]}}"#),
        r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#.to_string(),
    ])
}

/// A plain text answer, which ends the turn loop.
fn final_body() -> String {
    sse_response([
        r#"{"choices":[{"delta":{"content":"finished"}}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
    ])
}

/// Answer by what the request carries, not by how many have arrived: afi probes
/// the context window on the side, and a counter would hand the probe the reply
/// meant for the first turn. A body holding a tool result is the second turn.
fn reply_for(body: &str, target: &Path) -> String {
    if body.contains(r#""role":"tool""#) {
        final_body()
    } else {
        tool_call_body(target)
    }
}

fn serve(listener: TcpListener, target: &Path, bodies: &Bodies) -> JoinHandle<()> {
    let target = target.to_path_buf();
    let bodies = Arc::clone(bodies);
    thread::spawn(move || {
        while let Ok((stream, _)) = listener.accept() {
            answer(stream, &target, &bodies);
        }
    })
}

fn answer(mut stream: TcpStream, target: &Path, bodies: &Bodies) {
    let mut reader = BufReader::new(stream.try_clone().expect("the socket must clone"));
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    let body = read_request_body(&mut reader);
    let response = if request_line.contains("/chat/completions") {
        bodies
            .lock()
            .expect("the lock must hold")
            .push(body.clone());
        reply_for(&body, target)
    } else {
        NOT_FOUND.to_string()
    };
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// One-shot, auto-approving, against the fake endpoint. `--yolo` is deliberate:
/// with it, approval cannot be the reason a write does not happen, so the only
/// remaining explanation is the tool policy.
fn run_afi(home: &TempDir, addr: SocketAddr, extra: &[&str]) -> Output {
    let mut args = vec!["--yolo", "-f", "-"];
    args.extend_from_slice(extra);
    let mut child = Command::new(env!("CARGO_BIN_EXE_afi"))
        .args(&args)
        .env_clear()
        .env("AFI_HOME", home.path())
        .env("HOME", home.path())
        .env("AFI_BASE_URL", format!("http://{addr}/v1"))
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
        .write_all(b"write the file\n")
        .expect("the prompt must write");
    child.wait_with_output().expect("afi must exit")
}

/// The tool names in every request that advertised any, flattened.
fn advertised(bodies: &Bodies) -> Vec<String> {
    let bodies = bodies.lock().expect("the lock must hold");
    let mut names = Vec::new();
    for body in bodies.iter() {
        let Ok(parsed) = serde_json::from_str::<Value>(body) else {
            continue;
        };
        let Some(tools) = parsed.get("tools").and_then(Value::as_array) else {
            continue;
        };
        names.extend(
            tools
                .iter()
                .filter_map(|tool| tool.pointer("/function/name").and_then(Value::as_str))
                .map(str::to_string),
        );
    }
    names
}

/// The control, and the reason the test below means anything: the same endpoint
/// and the same call, with no posture, has to reach the filesystem.
fn the_same_run_writes_without_the_flag(addr: SocketAddr, target: &Path, bodies: &Bodies) {
    let home = TempDir::new().unwrap();
    let output = run_afi(&home, addr, &[]);
    assert!(
        target.exists(),
        "the control run must write the file, or this test proves nothing.\n\
         stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_file(target).expect("the control file must clear");
    assert!(
        advertised(bodies).iter().any(|name| name == "write_file"),
        "an unrestricted run must offer write_file"
    );
    bodies.lock().expect("the lock must hold").clear();
}

/// The request never offers a blocked schema, so a model has nothing to name in
/// the first place. The dispatch refusal is the other half.
fn no_blocked_schema_was_offered(bodies: &Bodies) {
    let offered = advertised(bodies);
    assert!(!offered.is_empty(), "the restricted run must offer tools");
    for name in ["write_file", "edit_file", "run_bash", "wait_background"] {
        assert!(
            !offered.contains(&name.to_string()),
            "{name} was advertised to a read-only run: {offered:?}"
        );
    }
}

#[test]
fn read_only_blocks_the_write_the_model_asks_for() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port must bind");
    let addr = listener.local_addr().expect("the port must be readable");
    let workspace = TempDir::new().unwrap();
    let target = workspace.path().join("written.txt");
    let bodies: Bodies = Arc::new(Mutex::new(Vec::new()));
    // Left to the process rather than joined: the thread parks in accept.
    let _server = serve(listener, &target, &bodies);

    the_same_run_writes_without_the_flag(addr, &target, &bodies);

    let restricted = TempDir::new().unwrap();
    let output = run_afi(&restricted, addr, &["--read-only"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !target.exists(),
        "--read-only must not let a write through.\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("blocked by policy"),
        "the refusal must be visible in the transcript: {stdout}"
    );
    no_blocked_schema_was_offered(&bodies);
}
