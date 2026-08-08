//! Registration, endpoint derivation, and the refusals for a Bedrock source
//! signing with a static key. The role-assuming half is in
//! `sources_bedrock_role`, split off only to keep both files under the
//! repository's per-file line cap.
//!
//! Every case goes through `common::build`, which takes an explicit env map and
//! never reads the real shell env or `~/.env`, so an `AWS_*` variable in the
//! developer's own environment cannot reach these.

mod common;

use afi::config::Protocol;
use common::bedrock_of;

use afi::summary::ErrorKind;

const LOCAL: (&str, &str) = ("AFI_SOURCE_LOCAL_BASE_URL", "http://localhost:8080/v1");
const KEY: (&str, &str) = ("AWS_ACCESS_KEY_ID", "AKIDEXAMPLE");
const SECRET: (&str, &str) = ("AWS_SECRET_ACCESS_KEY", "wJalrXUtnFEMI");
const REGION: (&str, &str) = ("AWS_REGION", "us-east-1");

// --- the built-in source ------------------------------------------------------

#[test]
fn registers_from_aws_credentials_with_the_region_as_the_endpoint() {
    let rt = common::build(&["afi"], &[LOCAL, KEY, SECRET, REGION]);
    let src = &rt.sources["bedrock"];
    assert_eq!(
        src.base_url,
        "https://bedrock-runtime.us-east-1.amazonaws.com/v1"
    );
    assert_eq!(src.model.as_deref(), Some("zai.glm-5"));
    assert!(src.protocol.is_bedrock());
    assert!(
        !src.is_anthropic(),
        "Bedrock speaks the OpenAI-compatible shape, not the Messages API"
    );
    assert!(src.config_error().is_none());
}

#[test]
fn is_absent_without_any_aws_credential() {
    let rt = common::build(&["afi"], &[LOCAL, REGION]);
    assert!(
        !rt.sources.contains_key("bedrock"),
        "a Region alone is not a credential"
    );
}

#[test]
fn registers_last_so_it_never_displaces_the_startup_default() {
    let rt = common::build(&["afi"], &[LOCAL, KEY, SECRET, REGION]);
    assert_eq!(rt.active.as_deref(), Some("local"));
    assert_eq!(rt.source_order.last().map(String::as_str), Some("bedrock"));
}

#[test]
fn the_session_token_is_carried_when_the_shell_has_one() {
    let rt = common::build(
        &["afi"],
        &[LOCAL, KEY, SECRET, REGION, ("AWS_SESSION_TOKEN", "sts")],
    );
    assert_eq!(
        bedrock_of(&rt, "bedrock").session_token.as_deref(),
        Some("sts")
    );
}

#[test]
fn the_model_and_endpoint_are_overridable() {
    let rt = common::build(
        &["afi"],
        &[
            LOCAL,
            KEY,
            SECRET,
            REGION,
            ("AFI_BEDROCK_MODEL", "qwen.qwen3-coder-30b-a3b-v1:0"),
            (
                "AFI_BEDROCK_BASE_URL",
                "https://bedrock-runtime.us-west-2.amazonaws.com/openai/v1",
            ),
        ],
    );
    let src = &rt.sources["bedrock"];
    assert_eq!(src.model.as_deref(), Some("qwen.qwen3-coder-30b-a3b-v1:0"));
    assert_eq!(
        src.base_url,
        "https://bedrock-runtime.us-west-2.amazonaws.com/openai/v1"
    );
}

/// Bedrock hosts many models, so a switch can name one, as with every other
/// multi-model source.
#[test]
fn a_source_switch_can_pin_any_of_the_open_weight_models() {
    let mut rt = common::build(&["afi"], &[LOCAL, KEY, SECRET, REGION]);
    assert!(rt.switch_source("bedrock", Some("moonshotai.kimi-k2.5")));
    assert_eq!(rt.model.as_deref(), Some("moonshotai.kimi-k2.5"));
}

// --- a source configured by hand ----------------------------------------------

#[test]
fn a_named_source_opts_in_with_the_protocol_value() {
    let rt = common::build(
        &["afi"],
        &[
            ("AFI_SOURCE_AWS_PROTOCOL", "aws-bedrock-openai"),
            ("AFI_SOURCE_AWS_MODEL", "openai.gpt-oss-20b-1:0"),
            KEY,
            SECRET,
            REGION,
        ],
    );
    let src = &rt.sources["aws"];
    assert!(src.protocol.is_bedrock());
    assert_eq!(src.model.as_deref(), Some("openai.gpt-oss-20b-1:0"));
    assert_eq!(
        src.base_url, "https://bedrock-runtime.us-east-1.amazonaws.com/v1",
        "a Bedrock source needs no BASE_URL; its Region names the endpoint"
    );
}

#[test]
fn a_named_source_keeps_a_base_url_it_was_given() {
    let rt = common::build(
        &["afi"],
        &[
            ("AFI_SOURCE_AWS_PROTOCOL", "aws-bedrock-openai"),
            ("AFI_SOURCE_AWS_BASE_URL", "https://gateway.internal/v1"),
            KEY,
            SECRET,
            REGION,
        ],
    );
    assert_eq!(rt.sources["aws"].base_url, "https://gateway.internal/v1");
}

/// Auto-discovery keys off `_BASE_URL`, which a Bedrock source may not have.
#[test]
fn a_protocol_alone_is_enough_to_be_discovered() {
    let rt = common::build(
        &["afi"],
        &[
            ("AFI_SOURCE_AWS_PROTOCOL", "aws-bedrock-openai"),
            KEY,
            SECRET,
            REGION,
        ],
    );
    assert!(rt.sources.contains_key("aws"));
    assert_eq!(rt.active.as_deref(), Some("aws"));
}

/// The same discovery change must not conjure a source out of a stray
/// `_PROTOCOL` on any other protocol, where a url is still required.
#[test]
fn a_protocol_alone_registers_nothing_on_the_other_protocols() {
    let rt = common::build(
        &["afi"],
        &[
            LOCAL,
            ("AFI_SOURCE_GHOST_PROTOCOL", "anthropic"),
            ("AFI_SOURCE_PHANTOM_PROTOCOL", "openai"),
        ],
    );
    assert!(!rt.sources.contains_key("ghost"));
    assert!(!rt.sources.contains_key("phantom"));
    assert_eq!(rt.source_order, ["local"]);
}

#[test]
fn a_typo_in_the_protocol_value_does_not_reach_bedrock() {
    let rt = common::build(
        &["afi"],
        &[
            ("AFI_SOURCE_AWS_BASE_URL", "https://gateway.internal/v1"),
            ("AFI_SOURCE_AWS_PROTOCOL", "aws-bedrock"),
            KEY,
            SECRET,
            REGION,
        ],
    );
    assert_eq!(rt.sources["aws"].protocol, Protocol::OpenAiCompat);
}

// --- refusals ------------------------------------------------------------------

#[test]
fn an_incomplete_credential_refuses_the_run_and_names_what_is_absent() {
    let rt = common::build(&["afi", "--source", "bedrock"], &[LOCAL, KEY, REGION]);
    let refusals = rt.refusals();
    assert_eq!(refusals.len(), 1, "got {refusals:?}");
    assert_eq!(
        refusals[0].message,
        "source bedrock signs for Bedrock but AWS_SECRET_ACCESS_KEY is not set"
    );
    // The kind travels with the refusal: no retry assembles a missing credential.
    assert_eq!(refusals[0].kind, ErrorKind::Auth);
}

#[test]
fn a_missing_region_is_named_too() {
    let rt = common::build(&["afi", "--source", "bedrock"], &[LOCAL, KEY, SECRET]);
    let refusals = rt.refusals();
    assert_eq!(refusals.len(), 1, "got {refusals:?}");
    assert_eq!(
        refusals[0].message,
        "source bedrock signs for Bedrock but AWS_REGION is not set"
    );
}

/// The startup default is a guess afi makes, so it skips a source that cannot
/// be used. Without this, `aws` sorts ahead of `local` on name alone and an
/// ordinary shell opened before `aws sso login` could not start afi at all -
/// against any source.
#[test]
fn an_unusable_source_is_not_chosen_as_the_startup_default() {
    let rt = common::build(
        &["afi"],
        &[LOCAL, ("AFI_SOURCE_AWS_PROTOCOL", "aws-bedrock-openai")],
    );
    assert_eq!(rt.source_order, ["aws", "local"], "both still exist");
    assert_eq!(rt.active.as_deref(), Some("local"));
    assert!(rt.refusals().is_empty(), "the run starts");
}

/// Asking for it by name is an instruction, not a guess, and is answered with
/// the refusal naming what is missing.
#[test]
fn asking_for_an_unusable_source_by_name_still_refuses() {
    let flag = common::build(
        &["afi", "--source", "aws"],
        &[LOCAL, ("AFI_SOURCE_AWS_PROTOCOL", "aws-bedrock-openai")],
    );
    assert_eq!(flag.active.as_deref(), Some("aws"));
    assert_eq!(flag.refusals().len(), 1, "got {:?}", flag.refusals());

    let env = common::build(
        &["afi"],
        &[
            LOCAL,
            ("AFI_SOURCE_AWS_PROTOCOL", "aws-bedrock-openai"),
            ("AFI_ACTIVE", "aws"),
        ],
    );
    assert_eq!(env.active.as_deref(), Some("aws"));
    assert_eq!(env.refusals().len(), 1, "got {:?}", env.refusals());
}

/// With nothing usable anywhere, the refusal still has to be reached - falling
/// back to the first source is what gets `AWS_REGION` named.
#[test]
fn the_only_source_is_still_chosen_when_it_cannot_be_used() {
    let rt = common::build(
        &["afi"],
        &[("AFI_SOURCE_AWS_PROTOCOL", "aws-bedrock-openai")],
    );
    assert_eq!(rt.active.as_deref(), Some("aws"));
    let refusals = rt.refusals();
    assert_eq!(refusals.len(), 1, "got {refusals:?}");
    assert_eq!(
        refusals[0].message,
        "source aws signs for Bedrock but AWS_REGION, AWS_ACCESS_KEY_ID, \
         and AWS_SECRET_ACCESS_KEY are not set"
    );
}

/// The Region lands in the endpoint host, so a value that is not a Region name
/// is refused rather than sending an `AWS_SESSION_TOKEN` to whatever host it
/// builds.
#[test]
fn a_region_that_is_not_a_region_name_refuses_the_run() {
    let rt = common::build(
        &["afi", "--source", "bedrock"],
        &[
            LOCAL,
            KEY,
            SECRET,
            ("AWS_REGION", "us-east-1.amazonaws.com.evil.test/x"),
        ],
    );
    let refusals = rt.refusals();
    assert_eq!(refusals.len(), 1, "got {refusals:?}");
    assert!(
        refusals[0].message.contains("is not a Region name"),
        "got {refusals:?}"
    );
}

/// A half-configured source nobody switched to costs the run nothing. Refusing
/// over one would make a stray `AWS_ACCESS_KEY_ID` in the shell block every run
/// against every other source.
#[test]
fn an_unused_bedrock_source_does_not_refuse_the_run() {
    let rt = common::build(&["afi"], &[LOCAL, KEY]);
    assert!(rt.sources.contains_key("bedrock"));
    assert_eq!(rt.active.as_deref(), Some("local"));
    assert!(rt.refusals().is_empty());
}

#[test]
fn a_complete_credential_refuses_nothing() {
    let rt = common::build(
        &["afi", "--source", "bedrock"],
        &[LOCAL, KEY, SECRET, REGION],
    );
    assert_eq!(rt.active.as_deref(), Some("bedrock"));
    assert!(rt.refusals().is_empty());
}
