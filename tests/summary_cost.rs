//! End-to-end: a real `afi` process, a real HTTP endpoint, and the cost figure
//! the run summary prints.
//!
//! Every piece of this is unit tested on its own - the rate table, the per-model
//! accumulator, the JSON assembly - and the wiring between them is what a unit
//! test cannot see. This runs the binary against a server that reports known
//! token counts and checks the money that comes out of stdout.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::thread::{self, JoinHandle};

use serde_json::Value;
use tempfile::TempDir;

/// Usage the fake endpoint reports, chosen so the arithmetic is checkable by
/// eye: a million tokens of each kind bills at exactly the per-million rate.
const PROMPT_TOKENS: u64 = 3_000_000;
const CACHED_TOKENS: u64 = 2_000_000;
const COMPLETION_TOKENS: u64 = 1_000_000;

/// `input` is `prompt_tokens` minus the cached subset, so this run spends 1M
/// input, 2M cache read, and 1M output.
const RATES: &str = r#"{"test-model": {"input": 3, "output": 15, "cache_read": 0.3}}"#;

/// 1M x $3 + 1M x $15 + 2M x $0.30.
const EXPECTED_USD: f64 = 18.6;

fn sse_body() -> String {
    let usage = format!(
        r#"{{"prompt_tokens":{PROMPT_TOKENS},"completion_tokens":{COMPLETION_TOKENS},"prompt_tokens_details":{{"cached_tokens":{CACHED_TOKENS}}}}}"#
    );
    [
        r#"data: {"choices":[{"delta":{"content":"done"}}]}"#.to_string(),
        format!(r#"data: {{"choices":[{{"delta":{{}},"finish_reason":"stop"}}],"usage":{usage}}}"#),
        "data: [DONE]".to_string(),
    ]
    .join("\n\n")
        + "\n\n"
}

/// Serve `count` requests, then stop. Anything that is not a chat completion
/// gets a 404, which is how a probe afi makes on the side is answered.
fn serve(listener: TcpListener, count: usize) -> JoinHandle<()> {
    thread::spawn(move || {
        for _ in 0..count {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
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
        let body = sse_body();
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    } else {
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

fn run_afi(
    home: &TempDir,
    addr: SocketAddr,
    prices: Option<&str>,
    summary_file: Option<&Path>,
) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_afi"));
    command
        .args(["--yolo", "--summary", "json", "-f", "-"])
        .env_clear()
        .env("AFI_HOME", home.path())
        .env("HOME", home.path())
        .env("AFI_BASE_URL", format!("http://{addr}/v1"))
        .env("AFI_MODEL", "test-model")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(prices) = prices {
        command.env("AFI_PRICES", prices);
    }
    if let Some(path) = summary_file {
        command.args(["--summary-file", path.to_str().expect("a utf-8 path")]);
    }
    let mut child = command.spawn().expect("afi must start");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(b"say done\n")
        .expect("the prompt must write");
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

#[test]
fn a_priced_run_reports_what_it_cost() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port must bind");
    let addr = listener.local_addr().expect("the port must be readable");
    // Two runs, and a little slack for any probe afi makes alongside them. The
    // thread is left to the process rather than joined: it is parked in accept.
    let _server = serve(listener, 8);
    let home = TempDir::new().unwrap();

    let priced = summary_of(&run_afi(&home, addr, Some(RATES), None));
    let usage = &priced["usage"];
    assert_eq!(usage["input_tokens"], PROMPT_TOKENS - CACHED_TOKENS);
    assert_eq!(usage["cache_read_tokens"], CACHED_TOKENS);
    assert_eq!(usage["output_tokens"], COMPLETION_TOKENS);
    assert_eq!(
        usage["cost_usd"], EXPECTED_USD,
        "each class must be billed at its own rate"
    );

    // Same run, no rate table: the counts stay, the money goes.
    let unpriced = summary_of(&run_afi(&home, addr, None, None));
    assert_eq!(unpriced["usage"]["total_tokens"], usage["total_tokens"]);
    assert!(
        unpriced["usage"].get("cost_usd").is_none(),
        "an unpriced run must carry no cost key at all"
    );
}

#[test]
fn a_run_that_worked_writes_the_same_object_to_the_summary_file() {
    // The `--summary-file` tests elsewhere all run against a closed port, which
    // leaves the case a workflow actually collects unproven: a run that answered,
    // its answer on the path, and the flag not turning a good run into a bad
    // exit code.
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port must bind");
    let addr = listener.local_addr().expect("the port must be readable");
    let _server = serve(listener, 4);
    let home = TempDir::new().unwrap();
    let path = home.path().join("run.json");

    let output = run_afi(&home, addr, Some(RATES), Some(&path));

    assert!(
        output.status.success(),
        "a written summary must not fail the run: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let body = fs::read_to_string(&path).expect("the summary file must exist");
    let written: Value = serde_json::from_str(&body).expect("the file must parse whole");
    assert_eq!(written["ok"], true);
    assert_eq!(written["answer"], "done");
    assert_eq!(written["usage"]["cost_usd"], EXPECTED_USD);
    // One object, two channels: they are rendered from a single build.
    assert_eq!(written, summary_of(&output));
}
