//! Shared test helpers: build a `Runtime` from an explicit env + args, with
//! no leakage from the real `~/.env` or shell env, plus the canned HTTP server
//! the end-to-end tests drive a real `afi` process against.
//!
//! The server pieces live here because every test that needs one needs the same
//! three: read a request without answering early, frame a reply as SSE, and pull
//! the summary back off stdout. What each test varies is which reply it sends,
//! which is the part that stays in the test.

use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;

use afi::Runtime;
use afi::config::{Bedrock, Protocol};
use serde_json::Value;
use tempfile::TempDir;

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

/// The credentials of a Bedrock source, or a panic naming what it is instead.
///
/// Here rather than in one of the two Bedrock test files because both need it:
/// the static-key cases and the role-assuming ones are separate binaries only
/// to stay under the per-file line cap, and unwrapping `Protocol::Bedrock`
/// twice is how they would drift.
#[allow(dead_code)]
pub fn bedrock_of(rt: &Runtime, name: &str) -> Bedrock {
    match &rt.sources[name].protocol {
        Protocol::Bedrock(bedrock) => (**bedrock).clone(),
        other => panic!("source {name} is on {other:?}, not Bedrock"),
    }
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

// --- the instruction-loading checkout -----------------------------------------
//
// Here rather than in one of the two `instructions*` test files because both need
// it: those files are separate binaries only to stay under the per-file line cap,
// and a second copy of the tree and the spawn is how the two would drift.

/// The rule a checkout's root `AGENTS.md` carries.
#[allow(dead_code)]
pub const ROOT_RULE: &str = "Run every command through mise.";
/// The rule a subtree's own `AGENTS.md` carries, which has to read last.
#[allow(dead_code)]
pub const DEEP_RULE: &str = "In this crate, never touch the generated bindings.";
/// The rule one directory above the checkout, which no walk may ever reach.
#[allow(dead_code)]
pub const OUTSIDE_RULE: &str = "Whatever the parent directory happens to say.";

/// A checkout with a `.git` marker, a root `AGENTS.md`, and a deeper one, inside a
/// directory that also holds an `AGENTS.md` the walk must never climb to.
#[allow(dead_code)]
pub fn checkout() -> TempDir {
    let outer = TempDir::new().expect("the temp dir must open");
    fs::write(outer.path().join("AGENTS.md"), OUTSIDE_RULE).expect("the outer file must write");
    let root = outer.path().join("repo");
    fs::create_dir_all(root.join(".git")).expect("the marker must write");
    fs::create_dir_all(root.join("crates/api")).expect("the subtree must write");
    fs::write(root.join("AGENTS.md"), ROOT_RULE).expect("the root file must write");
    fs::write(root.join("crates/api/AGENTS.md"), DEEP_RULE).expect("the deep file must write");
    outer
}

/// A [`checkout`] plus a source file inside `crates/api` for a tool call to name.
///
/// Here rather than in one of the `instructions*` test binaries because three of them
/// need it, and a second copy of the tree is how they would drift.
#[allow(dead_code)]
pub fn workspace() -> TempDir {
    let dir = checkout();
    fs::create_dir_all(repo(&dir).join("crates/api/src")).expect("the subtree must write");
    fs::write(repo(&dir).join("crates/api/src/lib.rs"), "pub fn go() {}\n")
        .expect("the source file must write");
    dir
}

/// The repository root inside a [`checkout`].
#[allow(dead_code)]
pub fn repo(dir: &TempDir) -> PathBuf {
    dir.path().join("repo")
}

/// One-shot afi run from `cwd`, which is what these tests are about: the walk
/// starts at the process's own working directory.
///
/// Points at `addr` when there is one and at a closed port when there is not - a
/// run that refuses to start never reaches either.
#[allow(dead_code)]
pub fn run_afi_in(
    home: &TempDir,
    cwd: &Path,
    addr: Option<SocketAddr>,
    extra: &[&str],
    env: &[(&str, &str)],
) -> Output {
    let mut args = vec!["-f", "-"];
    args.extend_from_slice(extra);
    spawn_afi_in(home, cwd, addr, &args, env, "review the diff\n")
}

/// A piped REPL session rather than a one-shot, which is the path that reads slash
/// commands off stdin.
#[allow(dead_code)]
pub fn repl_afi_in(
    home: &TempDir,
    cwd: &Path,
    addr: Option<SocketAddr>,
    extra: &[&str],
    input: &str,
) -> Output {
    spawn_afi_in(home, cwd, addr, extra, &[], input)
}

/// The one spawn every helper above funnels through, so a second copy cannot drift
/// and make the feature look like it misbehaved. Public for the test that needs its
/// own prompt text rather than a new copy of the twelve lines below.
#[allow(dead_code)]
pub fn spawn_afi_in(
    home: &TempDir,
    cwd: &Path,
    addr: Option<SocketAddr>,
    args: &[&str],
    env: &[(&str, &str)],
    input: &str,
) -> Output {
    let base = addr.map_or_else(
        || "http://127.0.0.1:9/v1".to_string(),
        |addr| format!("http://{addr}/v1"),
    );
    let mut command = Command::new(env!("CARGO_BIN_EXE_afi"));
    command
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("AFI_HOME", home.path())
        .env("HOME", home.path())
        .env("AFI_BASE_URL", base)
        .env("AFI_MODEL", "test-model")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in env {
        command.env(key, value);
    }
    let mut child = command.spawn().expect("afi must start");
    let written = child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(input.as_bytes());
    // A run that refuses its configured instructions exits before reading stdin,
    // so the pipe is already closed by the time this writes. That is what some of
    // these cases assert, not a failure of the harness.
    match written {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {}
        Err(error) => panic!("the prompt must write: {error}"),
    }
    child.wait_with_output().expect("afi must exit")
}

/// The project instruction paths a finished run's summary lists.
#[allow(dead_code)]
pub fn instruction_paths(output: &Output) -> Vec<String> {
    summary_of(output)["instructions"]
        .as_array()
        .expect("instructions is always an array")
        .iter()
        .map(|path| path.as_str().expect("a path").to_string())
        .collect()
}

/// The session id a finished run says to resume with.
#[allow(dead_code)]
pub fn session_of(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .split("resume with: afi --resume ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("the run must save a session: {stdout}"))
        .to_string()
}

/// A finished process's stderr, where every refusal is reported.
#[allow(dead_code)]
pub fn stderr_of(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("stderr must be utf-8")
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

/// Run the real `afi` binary on a clean environment and feed it `input`.
///
/// Every end-to-end summary test needs the same setup, and got it by copying it:
/// a home nothing else has written to, `env_clear` so the shell that started
/// `cargo test` cannot configure the run, three piped streams, the prompt
/// written to stdin, and the exit awaited. What a test actually varies is `args`
/// and `env`, so that is all it passes.
///
/// `home` comes from the caller rather than being made here, because a test that
/// runs the binary twice may want the second run to see what the first one saved.
#[allow(dead_code)]
pub fn run_afi(home: &Path, args: &[&str], env: &[(&str, &str)], input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_afi"))
        .args(args)
        .env_clear()
        .env("AFI_HOME", home)
        .env("HOME", home)
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
        .write_all(input.as_bytes())
        .expect("the input must write");
    child.wait_with_output().expect("afi must exit")
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
