//! End-to-end: what the run summary says about the credential that paid.
//!
//! The unit tests cover the mapping and the JSON. What they cannot see is the
//! wiring - that the block reaches stdout at all, that it names the credential
//! the tokens were billed to rather than whichever source the session ended on,
//! and that no credential rides along with it. A summary is uploaded as a build
//! artifact, which carries no masking, so a token that leaks here leaks in
//! plain text.

use std::process::Output;

use serde_json::Value;
use tempfile::TempDir;

mod common;
use common::{billing_server, run_afi, summary_of};

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

const BILLED_INPUT: u64 = 1000;
const BILLED_OUTPUT: u64 = 50;

/// Usage the fake endpoint reports, so a run has something to attribute.
fn billed_usage() -> String {
    format!(r#"{{"prompt_tokens":{BILLED_INPUT},"completion_tokens":{BILLED_OUTPUT}}}"#)
}

/// The federation variables plus whatever the case adds.
fn federated_env<'a>(extra: &[(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
    let mut env = FEDERATION.to_vec();
    env.extend_from_slice(extra);
    env
}

/// A federated source pointed at a port nothing is listening on.
fn unreachable_federated() -> Vec<(&'static str, &'static str)> {
    federated_env(&[
        ("AFI_ACTIVE", "anthropic"),
        ("AFI_ANTHROPIC_BASE_URL", UNREACHABLE),
    ])
}

// --- the run ------------------------------------------------------------------

fn run(args: &[&str], env: &[(&str, &str)], input: &str) -> Output {
    let home = TempDir::new().expect("a temporary home");
    let args: Vec<&str> = args.iter().copied().chain(["--summary", "json"]).collect();
    run_afi(home.path(), &args, env, input)
}

/// A one-shot run, which is the shape a CI job uses.
fn run_one_shot(env: &[(&str, &str)]) -> Output {
    run(&["--prompt-file", "-"], env, "review this\n")
}

// --- what the block reports ---------------------------------------------------

#[test]
fn a_federated_run_names_the_service_account_and_workspace_that_paid() {
    let output = run_one_shot(&unreachable_federated());
    let auth = &summary_of(&output)["auth"];
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
    let output = run_one_shot(&unreachable_federated());
    let stdout = String::from_utf8(output.stdout).expect("stdout must be utf-8");
    assert!(
        !stdout.contains(IDENTITY_TOKEN),
        "the summary leaked the identity token: {stdout}"
    );
    assert!(!stdout.contains("assertion"), "{stdout}");
    // stderr too, though nothing here is meant to print it. The token reaches
    // this process from `ANTHROPIC_IDENTITY_TOKEN`, and the exchange failure
    // path prints a server's response body verbatim - a CI job's log is no
    // better a place for a bearer credential than its artifacts are.
    let stderr = String::from_utf8(output.stderr).expect("stderr must be utf-8");
    assert!(
        !stderr.contains(IDENTITY_TOKEN),
        "the identity token reached the job log: {stderr}"
    );
}

#[test]
fn a_static_key_run_names_the_mode_and_stops() {
    let output = run_one_shot(&[
        ("AFI_ACTIVE", "anthropic"),
        ("AFI_ANTHROPIC_BASE_URL", UNREACHABLE),
        ("ANTHROPIC_API_KEY", "sk-ant-secret-value"),
    ]);
    let stdout = String::from_utf8(output.stdout.clone()).expect("stdout must be utf-8");
    let auth = &summary_of(&output)["auth"];
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
    let summary = summary_of(&output);
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
    let addr = billing_server(&billed_usage(), 12);
    let base_url = format!("http://{addr}/v1");
    let env = federated_env(&[
        ("AFI_ACTIVE", "local"),
        ("AFI_SOURCE_LOCAL_BASE_URL", base_url.as_str()),
        ("AFI_SOURCE_LOCAL_MODEL", "personal-model"),
        ("AFI_SOURCE_LOCAL_API_KEY", "sk-personal"),
        ("AFI_ANTHROPIC_BASE_URL", UNREACHABLE),
    ]);
    let output = run(&[], &env, "hi\n/source anthropic\n/quit\n");
    let summary = summary_of(&output);

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
    let addr = billing_server(&billed_usage(), 12);
    let base_url = format!("http://{addr}/v1");
    let output = run(
        &[],
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
    let summary = summary_of(&output);
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
