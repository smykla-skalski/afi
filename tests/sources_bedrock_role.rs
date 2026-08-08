//! Reaching Bedrock with no AWS key stored anywhere: a role assumed from an
//! OIDC identity token, and what refuses when the pieces are not there.
//!
//! Apart from `sources_bedrock`, which covers the static-key source, only
//! because the two together run past the repository's per-file line cap. Both
//! go through `common::build`, which takes an explicit env map and never reads
//! the real shell env or `~/.env`, so an `AWS_*` variable in the developer's own
//! environment cannot reach these.

mod common;

use afi::config::IdentitySource;
use common::bedrock_of;

use afi::summary::ErrorKind;

const LOCAL: (&str, &str) = ("AFI_SOURCE_LOCAL_BASE_URL", "http://localhost:8080/v1");
const KEY: (&str, &str) = ("AWS_ACCESS_KEY_ID", "AKIDEXAMPLE");
const SECRET: (&str, &str) = ("AWS_SECRET_ACCESS_KEY", "wJalrXUtnFEMI");
const REGION: (&str, &str) = ("AWS_REGION", "us-east-1");
const ROLE: (&str, &str) = ("AWS_ROLE_ARN", "arn:aws:iam::123456789012:role/afi-ci");
/// The two variables a workflow granting `permissions: id-token: write` gets.
const ACTIONS_URL: (&str, &str) = ("ACTIONS_ID_TOKEN_REQUEST_URL", "https://actions/token");
const ACTIONS_TOKEN: (&str, &str) = ("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "runtime-token");

/// The whole point: a workflow with `id-token: write`, a role, and a Region
/// gets a usable Bedrock source with no AWS key stored anywhere.
#[test]
fn a_workflow_identity_and_a_role_are_enough_to_reach_bedrock() {
    let rt = common::build(
        &["afi", "--source", "bedrock"],
        &[REGION, ROLE, ACTIONS_URL, ACTIONS_TOKEN],
    );
    let src = &rt.sources["bedrock"];
    assert_eq!(
        src.base_url,
        "https://bedrock-runtime.us-east-1.amazonaws.com/openai/v1"
    );
    assert_eq!(src.model.as_deref(), Some("zai.glm-5"));
    assert!(rt.refusals().is_empty(), "got {:?}", rt.refusals());
    assert_eq!(src.run_auth().mode(), "sigv4_web_identity");
}

/// A role ARN is a credential in the making, so the source registers on it
/// alone - otherwise the refusal below would have nothing to name.
#[test]
fn a_role_arn_alone_registers_the_source() {
    let rt = common::build(&["afi"], &[LOCAL, ROLE]);
    assert!(rt.sources.contains_key("bedrock"));
    assert_eq!(
        rt.active.as_deref(),
        Some("local"),
        "an unusable source is not the startup default"
    );
}

#[test]
fn a_role_with_no_identity_token_refuses_the_run_and_says_where_one_comes_from() {
    let rt = common::build(&["afi", "--source", "bedrock"], &[LOCAL, REGION, ROLE]);
    let refusals = rt.refusals();
    assert_eq!(refusals.len(), 1, "got {refusals:?}");
    assert!(
        refusals[0].message.contains("AWS_WEB_IDENTITY_TOKEN_FILE"),
        "got {refusals:?}"
    );
    assert!(
        refusals[0].message.contains("id-token: write"),
        "got {refusals:?}"
    );
    // No retry mints an identity token either.
    assert_eq!(refusals[0].kind, ErrorKind::Auth);
}

#[test]
fn a_role_that_is_not_an_arn_refuses_the_run() {
    let rt = common::build(
        &["afi", "--source", "bedrock"],
        &[
            LOCAL,
            REGION,
            ("AWS_ROLE_ARN", "afi-ci"),
            ACTIONS_URL,
            ACTIONS_TOKEN,
        ],
    );
    let refusals = rt.refusals();
    assert_eq!(refusals.len(), 1, "got {refusals:?}");
    assert!(
        refusals[0].message.contains("is not a role ARN"),
        "got {refusals:?}"
    );
}

/// Documented precedence, checked where an operator would hit it: keys in the
/// shell win, and the summary is what says so.
#[test]
fn static_keys_win_over_a_role_and_the_summary_reports_which() {
    let rt = common::build(
        &["afi", "--source", "bedrock"],
        &[LOCAL, KEY, SECRET, REGION, ROLE, ACTIONS_URL, ACTIONS_TOKEN],
    );
    let src = &rt.sources["bedrock"];
    assert!(rt.refusals().is_empty());
    assert!(bedrock_of(&rt, "bedrock").federating().is_none());
    assert_eq!(src.run_auth().mode(), "sigv4");
}

/// A named source opting in with `_PROTOCOL` reads the same AWS variables, so
/// it federates on the same terms as the built-in one.
#[test]
fn a_named_source_federates_too() {
    let rt = common::build(
        &["afi"],
        &[
            ("AFI_SOURCE_AWS_PROTOCOL", "aws-bedrock-openai"),
            REGION,
            ROLE,
            ACTIONS_URL,
            ACTIONS_TOKEN,
        ],
    );
    assert_eq!(rt.active.as_deref(), Some("aws"));
    assert!(rt.refusals().is_empty(), "got {:?}", rt.refusals());
    assert_eq!(rt.sources["aws"].run_auth().mode(), "sigv4_web_identity");
}

/// afi reads its own merged environment, so a token file named in `~/.env`
/// counts - reading the process env at request time would miss it.
#[test]
fn the_identity_comes_from_the_merged_environment() {
    let rt = common::build(
        &["afi", "--source", "bedrock"],
        &[
            LOCAL,
            REGION,
            ROLE,
            ("AWS_WEB_IDENTITY_TOKEN_FILE", "/var/run/secrets/token"),
        ],
    );
    assert!(rt.refusals().is_empty(), "got {:?}", rt.refusals());
    let web = bedrock_of(&rt, "bedrock")
        .federating()
        .cloned()
        .expect("with no key, the role is used");
    assert!(matches!(
        web.identity.map(|identity| identity.source),
        Some(IdentitySource::File(_))
    ));
}
