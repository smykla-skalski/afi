//! End-to-end: what the run summary says about the credential that paid.
//!
//! The unit tests cover the mapping and the JSON. What they cannot see is the
//! wiring - that the block reaches stdout at all, and that no credential rides
//! along with it. A summary is uploaded as a build artifact, which carries no
//! masking, so a token that leaks here leaks in plain text.

use std::io::Write;
use std::process::{Command, Output, Stdio};

use serde_json::Value;
use tempfile::TempDir;

/// Stands in for the OIDC assertion. Distinctive enough to grep the whole of
/// stdout for.
const IDENTITY_TOKEN: &str = "eyJhbGciOi.not-a-real-assertion.sig";
/// A closed port, so the run fails at once. The summary prints either way, and a
/// failed run is exactly the one an audit reads.
const UNREACHABLE: &str = "http://127.0.0.1:9";

const FEDERATED: &[(&str, &str)] = &[
    ("AFI_ACTIVE", "anthropic"),
    ("AFI_ANTHROPIC_BASE_URL", UNREACHABLE),
    ("ANTHROPIC_FEDERATION_RULE_ID", "fdrl_pr_review"),
    ("ANTHROPIC_ORGANIZATION_ID", "org_acme"),
    ("ANTHROPIC_SERVICE_ACCOUNT_ID", "svac_ci_bot"),
    ("ANTHROPIC_WORKSPACE_ID", "wrkspc_reviews"),
    ("ANTHROPIC_IDENTITY_TOKEN", IDENTITY_TOKEN),
];

fn run(env: &[(&str, &str)]) -> Output {
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
    let stdout = String::from_utf8(output.stdout.clone()).expect("stdout must be utf-8");
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|error| panic!("summary must parse: {error}, got {stdout:?}"))
}

#[test]
fn a_federated_run_names_the_service_account_and_workspace_that_paid() {
    let output = run(FEDERATED);
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
    let output = run(FEDERATED);
    let stdout = String::from_utf8(output.stdout).expect("stdout must be utf-8");
    assert!(
        !stdout.contains(IDENTITY_TOKEN),
        "the summary leaked the identity token: {stdout}"
    );
    assert!(!stdout.contains("assertion"), "{stdout}");
}

#[test]
fn a_static_key_run_names_the_mode_and_stops() {
    let output = run(&[
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
fn an_openai_compatible_run_reports_its_mode_too() {
    // Federation is the case the block exists for, but a summary whose `auth`
    // key appears only sometimes is one a consumer has to special-case.
    let output = run(&[("AFI_BASE_URL", UNREACHABLE), ("AFI_MODEL", "test-model")]);
    let summary = summary(&output);
    assert_eq!(summary["auth"]["mode"], "api_key");
    assert_eq!(summary["ok"], false, "the closed port must fail the run");
}
