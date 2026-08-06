//! End-to-end: what the run summary says about the credential that paid.
//!
//! The unit tests cover the mapping and the JSON. What they cannot see is the
//! wiring - that the block reaches stdout at all, that it names the credential
//! the tokens were billed to rather than whichever source the session ended on,
//! and that no credential rides along with it. A summary is uploaded as a build
//! artifact, which carries no masking, so a token that leaks here leaks in
//! plain text.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Command, Output, Stdio};
use std::thread::{self, JoinHandle};

use serde_json::Value;
use tempfile::TempDir;

/// Stands in for the OIDC assertion. Distinctive enough to grep the whole of
/// stdout for.
const IDENTITY_TOKEN: &str = "eyJhbGciOi.not-a-real-assertion.sig";
/// A closed port, so a run that must not reach a server fails at once. The
/// summary prints either way, and a failed run is exactly the one an audit reads.
const UNREACHABLE: &str = "http://127.0.0.1:9";

const FEDERATION: &[(&str, &str)] = &[
    ("ANTHROPIC_FEDERATION_RULE_ID", "fdrl_pr_review"),
    ("ANTHROPIC_ORGANIZATION_ID", "org_acme"),
    ("ANTHROPIC_SERVICE_ACCOUNT_ID", "svac_ci_bot"),
    ("ANTHROPIC_WORKSPACE_ID", "wrkspc_reviews"),
    ("ANTHROPIC_IDENTITY_TOKEN", IDENTITY_TOKEN),
];

// --- the run ------------------------------------------------------------------

fn run(env: &[(&str, &str)], input: &str) -> Output {
    let home = TempDir::new().expect("a temporary home");
    let mut child = Command::new(env!("CARGO_BIN_EXE_afi"))
        .args(["--summary", "json"])
        .env_clear()
        .env("AFI_HOME", home.path())
        .env("HOME", home.path())
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

/// A one-shot run against a source that cannot answer.
fn run_one_shot(env: &[(&str, &str)]) -> Output {
    let home = TempDir::new().expect("a temporary home");
    let mut child = Command::new(env!("CARGO_BIN_EXE_afi"))
        .args(["--prompt-file", "-", "--summary", "json"])
        .env_clear()
        .env("AFI_HOME", home.path())
        .env("HOME", home.path())
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
        .write_all(b"review this\n")
        .expect("the prompt must write");
    child.wait_with_output().expect("afi must exit")
}

fn summary(output: &Output) -> Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout
        .lines()
        .find(|line| line.trim_start().starts_with('{'))
        .unwrap_or_else(|| panic!("no JSON summary on stdout: {stdout}"));
    serde_json::from_str(line).expect("the summary must be JSON")
}

// --- a server that reports usage, so a run has something to bill --------------

const BILLED_INPUT: u64 = 1000;
const BILLED_OUTPUT: u64 = 50;

fn sse_body() -> String {
    let usage =
        format!(r#"{{"prompt_tokens":{BILLED_INPUT},"completion_tokens":{BILLED_OUTPUT}}}"#);
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
    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
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

fn billing_server() -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port must bind");
    let addr = listener.local_addr().expect("the port must be readable");
    // Room for the completions plus any probe afi makes alongside them. The
    // thread is left to the process rather than joined: it parks in accept.
    (addr, serve(listener, 12))
}

// --- what the block reports ---------------------------------------------------

#[test]
fn a_federated_run_names_the_service_account_and_workspace_that_paid() {
    let mut env = FEDERATION.to_vec();
    env.extend([
        ("AFI_ACTIVE", "anthropic"),
        ("AFI_ANTHROPIC_BASE_URL", UNREACHABLE),
    ]);
    let output = run_one_shot(&env);
    let auth = &summary(&output)["auth"];
    assert_eq!(auth["mode"], "federated");
    assert_eq!(auth["organization_id"], "org_acme");
    assert_eq!(auth["service_account_id"], "svac_ci_bot");
    assert_eq!(auth["workspace_id"], "wrkspc_reviews");
    assert_eq!(auth["federation_rule_id"], "fdrl_pr_review");
}

#[test]
fn the_identity_token_never_reaches_the_summary() {
    // The one thing this whole block must not do. The assertion is a bearer
    // credential in its own right and sits one field away from the ids that are
    // safe to publish.
    let mut env = FEDERATION.to_vec();
    env.extend([
        ("AFI_ACTIVE", "anthropic"),
        ("AFI_ANTHROPIC_BASE_URL", UNREACHABLE),
    ]);
    let output = run_one_shot(&env);
    let stdout = String::from_utf8(output.stdout).expect("stdout must be utf-8");
    assert!(
        !stdout.contains(IDENTITY_TOKEN),
        "the summary leaked the identity token: {stdout}"
    );
    assert!(!stdout.contains("assertion"), "{stdout}");
}

#[test]
fn a_static_key_run_names_the_mode_and_stops() {
    let output = run_one_shot(&[
        ("AFI_ACTIVE", "anthropic"),
        ("AFI_ANTHROPIC_BASE_URL", UNREACHABLE),
        ("ANTHROPIC_API_KEY", "sk-ant-secret-value"),
    ]);
    let stdout = String::from_utf8(output.stdout.clone()).expect("stdout must be utf-8");
    let auth = &summary(&output)["auth"];
    assert_eq!(auth["mode"], "api_key");
    assert!(
        auth.get("organization_id").is_none(),
        "a static key identifies nothing: {auth}"
    );
    assert!(
        !stdout.contains("sk-ant-secret-value"),
        "the summary leaked the api key: {stdout}"
    );
}

#[test]
fn a_keyless_local_server_reports_no_credential_rather_than_a_key() {
    // `Source::new` stores the `sk-noop` placeholder when nothing is configured,
    // and afi refuses to authenticate with it. Reporting `api_key` here would
    // attest to a credential the run never had.
    let output = run_one_shot(&[("AFI_BASE_URL", UNREACHABLE), ("AFI_MODEL", "test-model")]);
    let summary = summary(&output);
    assert_eq!(summary["auth"]["mode"], "none");
    assert_eq!(summary["ok"], false, "the closed port must fail the run");
}

// --- which credential actually paid -------------------------------------------

#[test]
fn the_credential_that_paid_is_reported_not_the_one_active_at_exit() {
    // The bug this guards: a session that spends on a personal key and then
    // switches to the federated source used to print the service account's ids
    // beside tokens it never bought - an unmasked artifact attesting to the
    // wrong budget.
    let (addr, _server) = billing_server();
    let base_url = format!("http://{addr}/v1");
    let mut env = FEDERATION.to_vec();
    env.extend([
        ("AFI_ACTIVE", "local"),
        ("AFI_SOURCE_LOCAL_BASE_URL", base_url.as_str()),
        ("AFI_SOURCE_LOCAL_MODEL", "personal-model"),
        ("AFI_SOURCE_LOCAL_API_KEY", "sk-personal"),
        ("AFI_ANTHROPIC_BASE_URL", UNREACHABLE),
    ]);
    let output = run(&env, "hi\n/source anthropic\n/quit\n");
    let summary = summary(&output);

    assert_eq!(
        summary["usage"]["input_tokens"], BILLED_INPUT,
        "the local source must have been billed: {summary}"
    );
    assert_eq!(
        summary["source"], "anthropic",
        "the session still ends on the source it switched to"
    );
    let auth = &summary["auth"];
    assert_eq!(auth["mode"], "api_key", "the credential that paid: {auth}");
    for federated_id in ["organization_id", "service_account_id", "workspace_id"] {
        assert!(
            auth.get(federated_id).is_none(),
            "{federated_id} names a budget that bought nothing: {auth}"
        );
    }
}

#[test]
fn two_credentials_that_both_spent_are_not_attributed_to_one() {
    // Neither answer is true, so the block reports none rather than picking.
    let (addr, _server) = billing_server();
    let base_url = format!("http://{addr}/v1");
    let output = run(
        &[
            ("AFI_ACTIVE", "first"),
            ("AFI_SOURCE_FIRST_BASE_URL", base_url.as_str()),
            ("AFI_SOURCE_FIRST_MODEL", "model-one"),
            ("AFI_SOURCE_FIRST_API_KEY", "sk-first"),
            ("AFI_SOURCE_SECOND_BASE_URL", base_url.as_str()),
            ("AFI_SOURCE_SECOND_MODEL", "model-two"),
            ("AFI_SOURCE_SECOND_API_KEY", "sk-second"),
        ],
        "hi\n/source second\nhi again\n/quit\n",
    );
    let summary = summary(&output);
    assert_eq!(
        summary["usage"]["requests"], 2,
        "both sources must have been billed: {summary}"
    );
    assert_eq!(
        summary["auth"],
        Value::Null,
        "no single credential paid for this run"
    );
}
