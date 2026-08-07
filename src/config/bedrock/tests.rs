//! Reading the AWS variables, and naming the ones that are absent.

use std::collections::HashMap;

use super::Bedrock;

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

const KEY: (&str, &str) = ("AWS_ACCESS_KEY_ID", "AKIDEXAMPLE");
const SECRET: (&str, &str) = ("AWS_SECRET_ACCESS_KEY", "wJalrXUtnFEMI");
const REGION: (&str, &str) = ("AWS_REGION", "us-east-1");

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

#[test]
fn the_region_names_the_endpoint() {
    let bedrock = Bedrock::from_env(&env(&[REGION]));
    assert_eq!(
        bedrock.base_url().as_deref(),
        Some("https://bedrock-runtime.us-east-1.amazonaws.com/v1")
    );
    assert_eq!(Bedrock::default().base_url(), None);
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
    ]));
    let rendered = format!("{bedrock:?}");
    assert!(!rendered.contains("wJalrXUtnFEMI"), "got {rendered}");
    assert!(!rendered.contains("session-token-value"), "got {rendered}");
    // The two that are not secret stay readable, or the dump is useless.
    assert!(rendered.contains("AKIDEXAMPLE"), "got {rendered}");
    assert!(rendered.contains("us-east-1"), "got {rendered}");
}
