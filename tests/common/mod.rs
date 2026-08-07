//! Shared test helpers: build a `Runtime` from an explicit env + args, with
//! no leakage from the real `~/.env` or shell env, plus the canned HTTP server
//! the end-to-end tests drive a real `afi` process against.
//!
//! The server pieces live here because every test that needs one needs the same
//! three: read a request without answering early, frame a reply as SSE, and pull
//! the summary back off stdout. What each test varies is which reply it sends,
//! which is the part that stays in the test.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::Output;
use std::thread;

use afi::Runtime;
use serde_json::Value;

/// The answer a context-window probe gets. afi asks on the side and falls back
/// when the endpoint is not there, so a canned server need not implement it.
#[allow(dead_code)]
pub const NOT_FOUND: &str =
    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

pub mod endpoint;

/// Build a runtime with a clean env (only the vars you pass in).
///
/// `args` is argv including argv[0] (typically `"afi"`). `env` is the
/// starting env; no `AFI_*` vars leak from the shell. `env_file` is
/// optional - pass `Some(path)` to exercise the `~/.env` loader.
#[allow(dead_code)]
pub fn build(args: &[&str], env: &[(&str, &str)]) -> Runtime {
    let args: Vec<String> = args.iter().map(ToString::to_string).collect();
    let env_map: HashMap<String, String> = env
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    Runtime::build(&args, env_map, None)
}

/// Build a runtime with an env file (the `~/.env` path).
#[allow(dead_code)]
pub fn build_with_env_file(
    args: &[&str],
    env: &[(&str, &str)],
    env_file: Option<&Path>,
) -> Runtime {
    let args: Vec<String> = args.iter().map(ToString::to_string).collect();
    let env_map: HashMap<String, String> = env
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    Runtime::build(&args, env_map, env_file)
}

/// Read past a request's headers and the body they announce, returning the body.
///
/// Draining matters even when the body is unwanted: answering before the client
/// has finished sending races the write, and the failure looks like a broken
/// stream rather than a test that replied too early.
#[allow(dead_code)]
pub fn read_request_body(reader: &mut BufReader<TcpStream>) -> String {
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

/// One HTTP response with an explicit length, so the client never waits on EOF.
#[allow(dead_code)]
pub fn http_response(status: &str, content_type: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// A 200 carrying `events` as an SSE stream, terminated the way a provider ends
/// one.
#[allow(dead_code)]
pub fn sse_response<I>(events: I) -> String
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
    http_response("200 OK", "text/event-stream", &body)
}

/// The run summary from a finished process's stdout.
///
/// Found by looking for the JSON line rather than by taking the last one: the
/// rendered run shares stdout, and a run that printed nothing else would be the
/// only case a positional rule got right.
#[allow(dead_code)]
pub fn summary_of(output: &Output) -> Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout
        .lines()
        .find(|line| line.trim_start().starts_with('{'))
        .unwrap_or_else(|| panic!("no JSON summary on stdout: {stdout}"));
    serde_json::from_str(line).expect("the summary must be JSON")
}

/// Bind a loopback port and answer `count` requests with one completion
/// reporting `usage`, then stop. Returns the address to point a source at.
///
/// Anything that is not a chat completion gets [`NOT_FOUND`], so `count` needs
/// slack above the completions a test expects - afi probes on the side.
///
/// The listener thread is detached rather than returned. It parks in `accept`,
/// so there is nothing useful to join, and parking does not hold up the test
/// binary's exit.
#[allow(dead_code)]
pub fn billing_server(usage: &str, count: usize) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port must bind");
    let addr = listener.local_addr().expect("the port must be readable");
    let body = sse_response([
        r#"{"choices":[{"delta":{"content":"done"}}]}"#.to_string(),
        format!(r#"{{"choices":[{{"delta":{{}},"finish_reason":"stop"}}],"usage":{usage}}}"#),
    ]);
    thread::spawn(move || {
        for _ in 0..count {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            answer(stream, &body);
        }
    });
    addr
}

fn answer(mut stream: TcpStream, body: &str) {
    let mut reader = BufReader::new(stream.try_clone().expect("the socket must clone"));
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
        return;
    }
    read_request_body(&mut reader);
    let response = if request_line.contains("/chat/completions") {
        body
    } else {
        NOT_FOUND
    };
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}
