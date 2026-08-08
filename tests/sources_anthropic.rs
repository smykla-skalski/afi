//! Registration and auth-precedence for the built-in `anthropic` source.
//!
//! Modelled on `tests/sources_builtin.rs`. Every case goes through
//! `common::build`, which takes an explicit env map and never reads the real
//! shell env or `~/.env`, so these run in parallel with everything else.

mod common;

use std::fs;
use std::path::PathBuf;

use afi::config::{ANTHROPIC_IDENTITY, Identity, IdentitySource, Protocol};

const LOCAL: (&str, &str) = ("AFI_SOURCE_LOCAL_BASE_URL", "http://localhost:8080/v1");

// --- registration -------------------------------------------------------------

#[test]
fn registers_from_an_api_key_with_sensible_defaults() {
    let rt = common::build(&["afi"], &[LOCAL, ("ANTHROPIC_API_KEY", "sk-ant-test")]);
    let src = &rt.sources["anthropic"];
    assert_eq!(src.base_url, "https://api.anthropic.com");
    assert_eq!(src.model.as_deref(), Some("claude-sonnet-5"));
    assert_eq!(src.api_key, "sk-ant-test");
    assert_eq!(src.protocol, Protocol::AnthropicApiKey);
}

#[test]
fn is_absent_without_any_credential() {
    let rt = common::build(&["afi"], &[LOCAL]);
    assert!(
        !rt.sources.contains_key("anthropic"),
        "an unusable source must not be registered"
    );
}

#[test]
fn registers_last_so_it_never_displaces_the_startup_default() {
    let rt = common::build(&["afi"], &[LOCAL, ("ANTHROPIC_API_KEY", "sk-ant-test")]);
    assert_eq!(rt.active.as_deref(), Some("local"));
    assert_eq!(
        rt.source_order.last().map(String::as_str),
        Some("anthropic")
    );
}

#[test]
fn the_afi_prefixed_key_takes_precedence_over_the_bare_one() {
    let rt = common::build(
        &["afi"],
        &[
            LOCAL,
            ("AFI_ANTHROPIC_API_KEY", "sk-afi"),
            ("ANTHROPIC_API_KEY", "sk-bare"),
        ],
    );
    assert_eq!(rt.sources["anthropic"].api_key, "sk-afi");
}

#[test]
fn model_and_base_url_are_overridable() {
    let rt = common::build(
        &["afi"],
        &[
            LOCAL,
            ("ANTHROPIC_API_KEY", "sk-ant-test"),
            ("AFI_ANTHROPIC_MODEL", "claude-opus-5"),
            ("AFI_ANTHROPIC_BASE_URL", "https://gateway.internal/v1"),
        ],
    );
    let src = &rt.sources["anthropic"];
    assert_eq!(src.model.as_deref(), Some("claude-opus-5"));
    assert_eq!(src.base_url, "https://gateway.internal/v1");
    assert_eq!(
        src.protocol,
        Protocol::AnthropicApiKey,
        "overriding the endpoint must not change the wire protocol"
    );
}

#[test]
fn the_builtin_overrides_avoid_the_source_namespace() {
    // `source_names` auto-discovers any `AFI_SOURCE_<NAME>_BASE_URL`, so if the
    // built-in read its overrides from that namespace, setting one would create
    // an OpenAiCompat source called `anthropic` holding the sk-noop placeholder
    // and the built-in would then short-circuit on "already registered".
    let rt = common::build(
        &["afi"],
        &[
            LOCAL,
            ("ANTHROPIC_API_KEY", "sk-ant-test"),
            ("AFI_ANTHROPIC_BASE_URL", "https://gateway.internal"),
        ],
    );
    assert_eq!(
        rt.source_order.iter().filter(|n| *n == "anthropic").count(),
        1
    );
    let src = &rt.sources["anthropic"];
    assert_eq!(src.protocol, Protocol::AnthropicApiKey);
    assert_eq!(src.api_key, "sk-ant-test", "must not be the placeholder");
}

#[test]
fn a_model_can_be_pinned_per_switch() {
    let mut rt = common::build(&["afi"], &[LOCAL, ("ANTHROPIC_API_KEY", "sk-ant-test")]);
    assert!(rt.switch_source("anthropic", Some("claude-haiku-4-5")));
    assert_eq!(rt.model.as_deref(), Some("claude-haiku-4-5"));
}

#[test]
fn an_explicit_source_block_overrides_the_builtin() {
    let rt = common::build(
        &["afi"],
        &[
            ("AFI_SOURCE_ANTHROPIC_BASE_URL", "https://proxy.internal/v1"),
            ("AFI_SOURCE_ANTHROPIC_API_KEY", "sk-explicit"),
            ("AFI_SOURCE_ANTHROPIC_MODEL", "claude-opus-5"),
            ("AFI_SOURCE_ANTHROPIC_PROTOCOL", "anthropic"),
            ("ANTHROPIC_API_KEY", "sk-should-be-ignored"),
        ],
    );
    let src = &rt.sources["anthropic"];
    assert_eq!(src.base_url, "https://proxy.internal/v1");
    assert_eq!(src.api_key, "sk-explicit");
    assert_eq!(src.protocol, Protocol::AnthropicApiKey);
}

#[test]
fn an_explicit_block_defaults_to_the_openai_protocol() {
    // Without an explicit PROTOCOL, a hand-configured source stays on the
    // OpenAI-compatible path - existing configs must not change meaning.
    let rt = common::build(
        &["afi"],
        &[("AFI_SOURCE_GATEWAY_BASE_URL", "https://gateway.internal/v1")],
    );
    assert_eq!(rt.sources["gateway"].protocol, Protocol::OpenAiCompat);
}

// --- auth precedence ----------------------------------------------------------

#[test]
fn a_bearer_token_is_used_when_no_api_key_is_set() {
    let rt = common::build(&["afi"], &[LOCAL, ("ANTHROPIC_AUTH_TOKEN", "oat-123")]);
    let src = &rt.sources["anthropic"];
    assert_eq!(src.protocol, Protocol::AnthropicOAuth);
    assert!(src.protocol.is_bearer());
    // The token rides in api_key; the client sends it as a bearer, never as
    // x-api-key.
    assert_eq!(src.api_key, "oat-123");
}

#[test]
fn an_api_key_wins_over_a_bearer_token() {
    let rt = common::build(
        &["afi"],
        &[
            LOCAL,
            ("ANTHROPIC_API_KEY", "sk-ant-test"),
            ("ANTHROPIC_AUTH_TOKEN", "oat-123"),
        ],
    );
    assert_eq!(rt.sources["anthropic"].protocol, Protocol::AnthropicApiKey);
}

#[test]
fn federation_is_used_when_no_static_credential_exists() {
    let rt = common::build(
        &["afi"],
        &[
            LOCAL,
            ("ANTHROPIC_FEDERATION_RULE_ID", "fdrl_1"),
            ("ANTHROPIC_ORGANIZATION_ID", "org-uuid"),
            ("ANTHROPIC_SERVICE_ACCOUNT_ID", "svac_1"),
            ("ANTHROPIC_WORKSPACE_ID", "wrkspc_1"),
        ],
    );
    let src = &rt.sources["anthropic"];
    assert!(src.protocol.is_bearer());
    let Protocol::AnthropicFederated(federation) = &src.protocol else {
        panic!("expected federated auth, got {:?}", src.protocol);
    };
    assert_eq!(federation.rule_id, "fdrl_1");
    assert_eq!(federation.workspace_id.as_deref(), Some("wrkspc_1"));
}

#[test]
fn federation_identity_from_an_env_file_reaches_the_source() {
    // The documented setup path: sources.example.env says "copy these lines into
    // ~/.env". Env-file values never reach the process env, so resolving the
    // identity source from `std::env` at mint time would silently miss this and
    // fail with a message naming the variable the user already set.
    let dir = tempfile::tempdir().unwrap();
    let env_file = dir.path().join("dotenv");
    fs::write(
        &env_file,
        "ANTHROPIC_FEDERATION_RULE_ID=fdrl_1\n\
         ANTHROPIC_ORGANIZATION_ID=org\n\
         ANTHROPIC_SERVICE_ACCOUNT_ID=svac\n\
         ANTHROPIC_IDENTITY_TOKEN_FILE=/run/secrets/oidc\n",
    )
    .unwrap();

    let rt = common::build_with_env_file(&["afi"], &[LOCAL], Some(&env_file));
    let Protocol::AnthropicFederated(federation) = &rt.sources["anthropic"].protocol else {
        panic!("expected federated auth from the env file");
    };
    assert_eq!(
        federation.identity,
        Some(Identity {
            vars: ANTHROPIC_IDENTITY,
            source: IdentitySource::File(PathBuf::from("/run/secrets/oidc")),
        }),
        "the identity source must come from the merged env map"
    );
}

#[test]
fn a_static_credential_wins_over_federation() {
    let rt = common::build(
        &["afi"],
        &[
            LOCAL,
            ("ANTHROPIC_AUTH_TOKEN", "oat-123"),
            ("ANTHROPIC_FEDERATION_RULE_ID", "fdrl_1"),
            ("ANTHROPIC_ORGANIZATION_ID", "org"),
            ("ANTHROPIC_SERVICE_ACCOUNT_ID", "svac"),
        ],
    );
    assert_eq!(rt.sources["anthropic"].protocol, Protocol::AnthropicOAuth);
}

#[test]
fn incomplete_federation_config_does_not_register() {
    let rt = common::build(
        &["afi"],
        &[
            LOCAL,
            ("ANTHROPIC_FEDERATION_RULE_ID", "fdrl_1"),
            ("ANTHROPIC_ORGANIZATION_ID", "org"),
            // no service account id
        ],
    );
    assert!(!rt.sources.contains_key("anthropic"));
}

#[test]
fn blank_credentials_count_as_unset() {
    let rt = common::build(
        &["afi"],
        &[
            LOCAL,
            ("ANTHROPIC_API_KEY", ""),
            ("ANTHROPIC_AUTH_TOKEN", ""),
        ],
    );
    assert!(!rt.sources.contains_key("anthropic"));
}

#[test]
fn a_credential_can_point_at_another_variable() {
    // `$NAME` indirection, consistent with the other built-in sources.
    let rt = common::build(
        &["afi"],
        &[
            LOCAL,
            ("ANTHROPIC_API_KEY", "$REAL_ANTHROPIC_KEY"),
            ("REAL_ANTHROPIC_KEY", "sk-indirect"),
        ],
    );
    assert_eq!(rt.sources["anthropic"].api_key, "sk-indirect");
}

// --- other sources are unaffected ---------------------------------------------

#[test]
fn openrouter_and_together_keep_the_openai_protocol() {
    let rt = common::build(
        &["afi"],
        &[
            LOCAL,
            ("AFI_TOGETHER_API_KEY", "t-key"),
            ("AFI_OPENROUTER_API_KEY", "or-key"),
            ("ANTHROPIC_API_KEY", "sk-ant-test"),
        ],
    );
    for name in ["local", "together", "openrouter"] {
        assert_eq!(
            rt.sources[name].protocol,
            Protocol::OpenAiCompat,
            "{name} must stay on the OpenAI path"
        );
    }
    assert_eq!(rt.sources["anthropic"].protocol, Protocol::AnthropicApiKey);
}
