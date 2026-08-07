//! End-to-end: a refused token exchange whose endpoint echoes the request back.
//!
//! The unit tests cover the cleaning. What they cannot see is whether the clean
//! body is the one that travels - a rejected grant reaches stderr, the JSON
//! summary on stdout, and the summary file a CI job uploads as a build artifact,
//! and the artifact is masked nowhere at all. afi fetches the identity token
//! from the Actions endpoint itself rather than through the toolkit that would
//! register it for masking, so if it survives this path it survives to the log
//! in plain text, and whoever reads the log can mint an access token with it.
//!
//! So this drives the real binary against an endpoint that answers the exchange
//! by quoting the request it turned down.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Command, Output, Stdio};
use std::thread::{self, JoinHandle};

use serde_json::Value;
use tempfile::TempDir;

/// Stands in for the OIDC assertion: three base64url segments, and distinctive
/// enough to grep a whole job log for.
const ASSERTION: &str =
    "eyJhbGciOiJSUzI1NiIsImtpZCI6ImFmaSJ9.eyJzdWIiOiJyZXBvOmFjbWUvYWZpIn0.c2lnbmF0dXJl";

const FEDERATION: &[(&str, &str)] = &[
    ("ANTHROPIC_FEDERATION_RULE_ID", "fdrl_pr_review"),
    ("ANTHROPIC_ORGANIZATION_ID", "org_acme"),
    ("ANTHROPIC_SERVICE_ACCOUNT_ID", "svac_ci_bot"),
    ("ANTHROPIC_IDENTITY_TOKEN", ASSERTION),
];

// --- an endpoint that echoes what it refused -----------------------------------

/// Refuse every exchange with a 400 that quotes the request body, the way a
/// proxy that reflects its input does. The thread is left to the process rather
/// than joined: it parks in `accept`.
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
    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
        return;
    }
    let request_body = read_body(&mut reader);
    let body = format!(
        r#"{{"type":"error","error":{{"type":"invalid_request_error","message":"the assertion did not satisfy the federation rule"}},"request":{request_body}}}"#
    );
    let response = format!(
        "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// Read past the headers and return whatever body they announce, so the client
/// is not answered before it has finished sending.
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
    if reader.read_exact(&mut body).is_err() {
        return "null".to_string();
    }
    String::from_utf8_lossy(&body).into_owned()
}

// --- the run -------------------------------------------------------------------

/// What one refused run reported, in the three places it reports it.
struct Reported {
    stdout: String,
    stderr: String,
    file: String,
}

impl Reported {
    /// Every place the failure reached. A credential has to be absent from all of
    /// them, not merely from whichever one a test remembered to check.
    fn everywhere(&self) -> String {
        format!("{}\n{}\n{}", self.stdout, self.stderr, self.file)
    }

    fn summary(&self) -> Value {
        let line = self
            .stdout
            .lines()
            .find(|line| line.trim_start().starts_with('{'))
            .unwrap_or_else(|| panic!("no JSON summary on stdout: {}", self.stdout));
        serde_json::from_str(line).expect("the summary must be JSON")
    }

    fn error(&self) -> String {
        self.summary()["error"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }
}

/// One prompt against a federated source whose exchange refuses it.
fn refused_run() -> Reported {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port must bind");
    let addr: SocketAddr = listener.local_addr().expect("the port must be readable");
    let _server = serve(listener);

    let home = TempDir::new().expect("a temporary home");
    let summary_file = home.path().join("summary.json");
    let base_url = format!("http://{addr}");
    let mut child = Command::new(env!("CARGO_BIN_EXE_afi"))
        .args(["--prompt-file", "-", "--summary", "json", "--summary-file"])
        .arg(&summary_file)
        .env_clear()
        .env("AFI_HOME", home.path())
        .env("HOME", home.path())
        .env("AFI_ACTIVE", "anthropic")
        .env("AFI_ANTHROPIC_BASE_URL", &base_url)
        .envs(FEDERATION.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("afi must start");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(b"review this\n")
        .expect("the prompt must write");
    let output: Output = child.wait_with_output().expect("afi must exit");

    Reported {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        file: fs::read_to_string(&summary_file).unwrap_or_default(),
    }
}

// --- what it reports -----------------------------------------------------------

/// The longest run of the assertion that appears anywhere in `text`.
///
/// Searching for the whole token is not enough: the reported body is cut to a
/// preview, so a leak arrives already shortened, and a check for the full string
/// passes on a body that quoted most of it. Every window of this length is
/// tried instead.
fn longest_leak(text: &str) -> Option<&'static str> {
    const WINDOW: usize = 16;
    (WINDOW..=ASSERTION.len())
        .rev()
        .flat_map(|len| (0..=ASSERTION.len() - len).map(move |at| &ASSERTION[at..at + len]))
        .find(|piece| text.contains(piece))
}

#[test]
fn a_refused_exchange_reports_the_assertion_nowhere() {
    let reported = refused_run();
    let everywhere = reported.everywhere();
    assert_eq!(
        longest_leak(&everywhere),
        None,
        "the run reported the identity token: {everywhere}"
    );
}

#[test]
fn the_summary_file_is_covered_by_the_same_pass() {
    // The file is the one a workflow uploads as a build artifact, where nothing
    // masks anything. It has to be non-empty, or the assertion above passes for
    // the wrong reason.
    let reported = refused_run();
    assert!(
        reported.file.contains("\"ok\""),
        "the summary file must hold a summary: {}",
        reported.file
    );
    assert_eq!(longest_leak(&reported.file), None, "{}", reported.file);
}

#[test]
fn what_was_removed_is_named_where_it_stood() {
    // Truncation marks itself too. Without a distinct marker a reader cannot tell
    // a credential that was struck from a body that merely ran long.
    let reported = refused_run();
    let error = reported.error();
    assert!(
        error.contains("[redacted OIDC identity token]"),
        "{error} / {}",
        reported.stderr
    );
}

#[test]
fn the_reason_for_the_refusal_survives() {
    // A rejected credential and a rate limit arrive the same way, and the type
    // and message are what tell them apart. Blanking the body would lose that.
    let reported = refused_run();
    let error = reported.error();
    assert!(error.contains("invalid_request_error"), "{error}");
    assert!(
        error.contains("did not satisfy the federation rule"),
        "{error}"
    );
}

#[test]
fn a_refused_exchange_is_still_an_auth_failure() {
    // Redaction rewrites the body, and the classification is read off the status
    // rather than the text - so it has to survive the rewrite.
    let reported = refused_run();
    assert_eq!(reported.summary()["ok"], false);
    assert_eq!(reported.summary()["error_kind"], "auth");
}
