//! Reading the AWS variables, choosing between the two credential modes, and
//! naming what is absent from whichever one applies.

use std::collections::HashMap;

use super::{Bedrock, dns_suffix};
use crate::config::IdentitySource;

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

const KEY: (&str, &str) = ("AWS_ACCESS_KEY_ID", "AKIDEXAMPLE");
const SECRET: (&str, &str) = ("AWS_SECRET_ACCESS_KEY", "wJalrXUtnFEMI");
const REGION: (&str, &str) = ("AWS_REGION", "us-east-1");
const ROLE: (&str, &str) = ("AWS_ROLE_ARN", "arn:aws:iam::123456789012:role/afi-ci");
/// What a workflow granting `permissions: id-token: write` exports.
const ACTIONS: [(&str, &str); 2] = [
    ("ACTIONS_ID_TOKEN_REQUEST_URL", "https://actions/token"),
    ("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "runtime-token"),
];

#[test]
fn reads_the_standard_variables() {
    let bedrock = Bedrock::from_env(&env(&[
        KEY,
        SECRET,
        REGION,
        ("AWS_SESSION_TOKEN", "session"),
    ]));
    assert_eq!(bedrock.region.as_deref(), Some("us-east-1"));
    assert_eq!(bedrock.access_key_id.as_deref(), Some("AKIDEXAMPLE"));
    assert_eq!(bedrock.secret_access_key.as_deref(), Some("wJalrXUtnFEMI"));
    assert_eq!(bedrock.session_token.as_deref(), Some("session"));
    assert_eq!(bedrock.missing(), Vec::<&str>::new());
    assert!(bedrock.incomplete("bedrock").is_none());
}

#[test]
fn aws_region_wins_over_aws_default_region() {
    let bedrock = Bedrock::from_env(&env(&[REGION, ("AWS_DEFAULT_REGION", "eu-west-1")]));
    assert_eq!(bedrock.region.as_deref(), Some("us-east-1"));
}

#[test]
fn aws_default_region_is_the_fallback() {
    let bedrock = Bedrock::from_env(&env(&[("AWS_DEFAULT_REGION", "eu-west-1")]));
    assert_eq!(bedrock.region.as_deref(), Some("eu-west-1"));
}

/// A variable exported as empty is how a shell spells "unset" in practice, and
/// signing with it would fail on the wire instead of at startup.
#[test]
fn blank_and_whitespace_values_count_as_absent() {
    let bedrock = Bedrock::from_env(&env(&[
        ("AWS_REGION", "  "),
        ("AWS_ACCESS_KEY_ID", ""),
        SECRET,
    ]));
    assert_eq!(bedrock.missing(), ["AWS_REGION", "AWS_ACCESS_KEY_ID"]);
}

#[test]
fn a_session_token_is_never_required() {
    let bedrock = Bedrock::from_env(&env(&[KEY, SECRET, REGION]));
    assert!(bedrock.session_token.is_none());
    assert!(
        bedrock.incomplete("bedrock").is_none(),
        "a long-lived IAM user has no session token"
    );
}

#[test]
fn one_missing_variable_reads_as_a_sentence() {
    let bedrock = Bedrock::from_env(&env(&[KEY, SECRET]));
    assert_eq!(
        bedrock.incomplete("bedrock").unwrap(),
        "source bedrock signs for Bedrock but AWS_REGION is not set"
    );
}

#[test]
fn two_missing_variables_read_as_a_sentence() {
    let bedrock = Bedrock::from_env(&env(&[KEY]));
    assert_eq!(
        bedrock.incomplete("aws").unwrap(),
        "source aws signs for Bedrock but AWS_REGION and AWS_SECRET_ACCESS_KEY are not set"
    );
}

#[test]
fn three_missing_variables_read_as_a_sentence() {
    assert_eq!(
        Bedrock::default().incomplete("aws").unwrap(),
        "source aws signs for Bedrock but AWS_REGION, AWS_ACCESS_KEY_ID, \
         and AWS_SECRET_ACCESS_KEY are not set"
    );
}

/// The path is pinned, not just the host. `/v1` shipped once and Bedrock serves
/// nothing there - it answers `UnknownOperationException` for every model - so
/// the whole url is asserted rather than the Region interpolation alone.
#[test]
fn the_region_names_the_endpoint() {
    let bedrock = Bedrock::from_env(&env(&[REGION]));
    assert_eq!(
        bedrock.base_url().as_deref(),
        Some("https://bedrock-runtime.us-east-1.amazonaws.com/openai/v1")
    );
    assert_eq!(Bedrock::default().base_url(), None);
}

/// The Region also names the partition, and one partition does not answer to the
/// commercial suffix. `GovCloud` does, so the `cn-` prefix is the whole rule -
/// asserted here because the alternative to naming it is a DNS failure that says
/// nothing about which of the two names was wrong.
#[test]
fn a_china_region_gets_the_china_suffix() {
    assert_eq!(dns_suffix("cn-north-1"), "amazonaws.com.cn");
    assert_eq!(dns_suffix("us-gov-west-1"), "amazonaws.com");
    assert_eq!(dns_suffix("us-east-1"), "amazonaws.com");
    let bedrock = Bedrock::from_env(&env(&[("AWS_REGION", "cn-north-1")]));
    assert_eq!(
        bedrock.base_url().as_deref(),
        Some("https://bedrock-runtime.cn-north-1.amazonaws.com.cn/openai/v1")
    );
}

#[test]
fn half_a_credential_still_counts_as_one() {
    // Enough to register the built-in source, which is what gets the missing
    // half named rather than silently dropped.
    assert!(Bedrock::from_env(&env(&[KEY])).has_any_credential());
    assert!(Bedrock::from_env(&env(&[SECRET])).has_any_credential());
    assert!(!Bedrock::from_env(&env(&[REGION])).has_any_credential());
    assert!(!Bedrock::default().has_any_credential());
}

/// `Source` and `Runtime` both derive `Debug`, so anything printed from one
/// carries this struct with it.
#[test]
fn debug_output_holds_no_secret() {
    let bedrock = Bedrock::from_env(&env(&[
        KEY,
        SECRET,
        REGION,
        ("AWS_SESSION_TOKEN", "session-token-value"),
        ("AWS_WEB_IDENTITY_TOKEN", "eyJhbGciOi.secret-assertion"),
        ROLE,
    ]));
    let rendered = format!("{bedrock:?}");
    assert!(!rendered.contains("wJalrXUtnFEMI"), "got {rendered}");
    assert!(!rendered.contains("session-token-value"), "got {rendered}");
    assert!(!rendered.contains("secret-assertion"), "got {rendered}");
    // The three that are not secret stay readable, or the dump is useless.
    assert!(rendered.contains("AKIDEXAMPLE"), "got {rendered}");
    assert!(rendered.contains("us-east-1"), "got {rendered}");
    assert!(rendered.contains("role/afi-ci"), "got {rendered}");
}

// --- assuming a role instead of holding a key ---------------------------------

/// The shape this feature exists for: a workflow with `id-token: write`, a role
/// ARN, and a Region. No key is stored anywhere.
#[test]
fn a_role_and_a_workflow_identity_are_a_whole_credential() {
    let bedrock = Bedrock::from_env(&env(&[REGION, ROLE, ACTIONS[0], ACTIONS[1]]));
    assert!(bedrock.has_any_credential(), "the source has to register");
    assert!(bedrock.incomplete("bedrock").is_none());
    let web = bedrock.federating().expect("with no key, the role is used");
    assert_eq!(web.role_arn, "arn:aws:iam::123456789012:role/afi-ci");
    assert_eq!(web.session_name, "afi");
    assert!(matches!(
        web.identity.as_ref().map(|identity| &identity.source),
        Some(IdentitySource::GithubActions { .. })
    ));
}

#[test]
fn the_session_name_is_overridable() {
    let bedrock = Bedrock::from_env(&env(&[
        REGION,
        ROLE,
        ("AWS_ROLE_SESSION_NAME", "afi-pr-review"),
        ACTIONS[0],
        ACTIONS[1],
    ]));
    assert_eq!(
        bedrock.web_identity.as_ref().unwrap().session_name,
        "afi-pr-review"
    );
}

/// The variable every AWS SDK reads, which an EKS pod identity and
/// `configure-aws-credentials` both set.
#[test]
fn a_web_identity_token_file_is_read_as_the_identity() {
    let bedrock = Bedrock::from_env(&env(&[
        REGION,
        ROLE,
        ("AWS_WEB_IDENTITY_TOKEN_FILE", "/var/run/secrets/token"),
    ]));
    assert!(bedrock.incomplete("bedrock").is_none());
    assert!(matches!(
        bedrock
            .federating()
            .and_then(|web| web.identity.as_ref())
            .map(|identity| &identity.source),
        Some(IdentitySource::File(_))
    ));
}

/// Every AWS SDK's default chain resolves environment keys ahead of a web
/// identity, and so does afi's own `anthropic` built-in across its three modes.
#[test]
fn a_complete_static_pair_wins_over_a_role() {
    let bedrock = Bedrock::from_env(&env(&[KEY, SECRET, REGION, ROLE, ACTIONS[0], ACTIONS[1]]));
    assert!(bedrock.web_identity.is_some(), "the role is still read");
    assert!(
        bedrock.federating().is_none(),
        "but the static pair is what signs"
    );
    assert_eq!(bedrock.run_auth().mode(), "sigv4");
}

/// A misspelled `AWS_SECRET_ACCESS_KEY` must not take down a run that had a
/// perfectly good role to assume - the SDK chain moves on from half a pair too.
#[test]
fn half_a_static_pair_does_not_win() {
    let bedrock = Bedrock::from_env(&env(&[KEY, REGION, ROLE, ACTIONS[0], ACTIONS[1]]));
    assert!(bedrock.federating().is_some());
    assert!(
        bedrock.incomplete("bedrock").is_none(),
        "the dangling key is not required of a federating source"
    );
    assert_eq!(bedrock.run_auth().mode(), "sigv4_web_identity");
}

/// A role ARN alone is a credential in the making, and the source has to exist
/// for the refusal below to name it.
#[test]
fn a_role_arn_alone_registers_the_source() {
    assert!(Bedrock::from_env(&env(&[ROLE])).has_any_credential());
}

// --- refusals on the federated path -------------------------------------------

#[test]
fn a_role_with_no_identity_token_refuses_and_says_where_one_comes_from() {
    let bedrock = Bedrock::from_env(&env(&[REGION, ROLE]));
    let refusal = bedrock.incomplete("bedrock").unwrap();
    assert!(
        refusal.starts_with("source bedrock assumes an AWS role but"),
        "got {refusal}"
    );
    assert!(refusal.contains("AWS_WEB_IDENTITY_TOKEN_FILE"), "{refusal}");
    assert!(refusal.contains("id-token: write"), "{refusal}");
}

/// A role name where its ARN belongs is the mistake worth catching: STS answers
/// it with a `ValidationError` about a request the operator never wrote.
#[test]
fn a_role_that_is_not_an_arn_refuses_and_names_the_variable() {
    let bedrock = Bedrock::from_env(&env(&[
        REGION,
        ("AWS_ROLE_ARN", "afi-ci"),
        ACTIONS[0],
        ACTIONS[1],
    ]));
    assert_eq!(
        bedrock.incomplete("bedrock").unwrap(),
        "source bedrock assumes an AWS role but AWS_ROLE_ARN=\"afi-ci\" is not a role ARN"
    );
}

/// Every partition, and a role kept under a path.
#[test]
fn the_other_role_arn_shapes_are_accepted() {
    for arn in [
        "arn:aws:iam::123456789012:role/afi-ci",
        "arn:aws-us-gov:iam::123456789012:role/afi-ci",
        "arn:aws-cn:iam::123456789012:role/afi-ci",
        "arn:aws:iam::123456789012:role/team/ci/afi",
    ] {
        let bedrock = Bedrock::from_env(&env(&[
            REGION,
            ("AWS_ROLE_ARN", arn),
            ACTIONS[0],
            ACTIONS[1],
        ]));
        assert!(bedrock.incomplete("bedrock").is_none(), "{arn}");
    }
}

/// The signature is still scoped to a Region, however the credential was
/// obtained - but the static pair is not, so it must not be named.
#[test]
fn a_federating_source_still_needs_a_region_and_nothing_else() {
    let bedrock = Bedrock::from_env(&env(&[ROLE, ACTIONS[0], ACTIONS[1]]));
    assert_eq!(bedrock.missing(), ["AWS_REGION"]);
    assert_eq!(
        bedrock.incomplete("bedrock").unwrap(),
        "source bedrock signs for Bedrock but AWS_REGION is not set"
    );
}
