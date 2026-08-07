//! A run that never produces an answer must not exit 0 claiming it did.
//!
//! The forced-final turn is the last thing a run does, and an empty one used to
//! return DONE: the process exited 0 and the summary carried `"answer": ""`,
//! which downstream reads as "the model had nothing to say" rather than "afi
//! never got anything". Raising the Anthropic `max_tokens` floor makes the
//! usual cause - a budget spent entirely on reasoning - much rarer, but the
//! reporting has to hold whatever the cause, so this asks the process.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Command, Output, Stdio};
use std::thread::{self, JoinHandle};

use serde_json::Value;
use tempfile::TempDir;

/// A stream that closes with no content and no tool call - the shape a turn
/// takes when the whole token budget went on reasoning.
fn empty_body() -> String {
    [
        r#"data: {"choices":[{"delta":{}}]}"#,
        r#"data: {"choices":[{"delta":{},"finish_reason":"length"}]}"#,
        "data: [DONE]",
    ]
    .join("\n\n")
        + "\n\n"
}

fn serve(listener: TcpListener) -> JoinHandle<()> {
    thread::spawn(move || {
        while let Ok((stream, _)) = listener.accept() {
            answer(stream);
        }
    })
}

fn answer(mut stream: TcpStream) {
    let mut reader = BufReader::new(stream.try_clone().expect("the socket must clone"));
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    drain_body(&mut reader);
    let response = if request_line.contains("/chat/completions") {
        let sse = empty_body();
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

fn run_afi(home: &TempDir, addr: SocketAddr) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_afi"))
        .args(["--yolo", "--summary", "json", "-f", "-"])
        .env_clear()
        .env("AFI_HOME", home.path())
        .env("HOME", home.path())
        .env("AFI_BASE_URL", format!("http://{addr}/v1"))
        .env("AFI_MODEL", "test-model")
        // One nudge, so the run reaches its forced-final turn quickly.
        .env("AFI_EMPTY_TURN_RETRY_LIMIT", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("afi must start");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(b"summarize the repository\n")
        .expect("the prompt must be written");
    child.wait_with_output().expect("afi must finish")
}

#[test]
fn a_run_that_never_answers_fails_instead_of_reporting_an_empty_answer() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port must bind");
    let addr = listener.local_addr().expect("the address must resolve");
    let server = serve(listener);
    let home = TempDir::new().expect("a temp home must exist");

    let output = run_afi(&home, addr);
    let stdout = String::from_utf8(output.stdout).expect("stdout must be utf-8");
    let stderr = String::from_utf8(output.stderr).expect("stderr must be utf-8");

    let summary: Value = stdout
        .lines()
        .find_map(|line| serde_json::from_str(line).ok())
        .unwrap_or_else(|| panic!("the summary must be on stdout, got: {stdout}"));
    assert_eq!(summary["ok"], false, "{summary}");
    assert_eq!(summary["answer"], "", "{summary}");
    assert_eq!(
        output.status.code(),
        Some(1),
        "a run with no answer exits 1"
    );
    assert!(
        stderr.contains("FORCED FINAL RETURNED NO ANSWER"),
        "the reason must be on stderr, got: {stderr}"
    );

    drop(server);
}
