use super::*;
use crate::config::NOOP_KEY;
use crate::summary::ErrorKind;

fn federation() -> Federation {
    Federation {
        rule_id: "fdrl_1".to_string(),
        organization_id: "org".to_string(),
        service_account_id: "svac".to_string(),
        workspace_id: None,
        identity: None,
    }
}

fn federated_protocol() -> Protocol {
    Protocol::AnthropicFederated(Box::new(federation()))
}

// --- headers ------------------------------------------------------------------

#[test]
fn api_key_mode_sends_x_api_key_and_no_bearer() {
    let map = auth_headers(&Protocol::AnthropicApiKey, "sk-ant-real", None).unwrap();
    assert_eq!(map["x-api-key"], "sk-ant-real");
    assert_eq!(map["anthropic-version"], ANTHROPIC_VERSION);
    assert!(!map.contains_key("authorization"));
    assert!(!map.contains_key("anthropic-beta"));
}

/// The load-bearing assertion of this whole change. A non-empty `x-api-key`
/// overrides the bearer and 401s, so the header must be absent entirely.
#[test]
fn oauth_mode_never_sends_x_api_key() {
    let map = auth_headers(&Protocol::AnthropicOAuth, NOOP_KEY, Some("oat-token")).unwrap();
    assert!(
        !map.contains_key("x-api-key"),
        "x-api-key must not be present in bearer mode at any value"
    );
    assert_eq!(map["authorization"], "Bearer oat-token");
    assert_eq!(map["anthropic-beta"], OAUTH_BETA);
    assert_eq!(map["anthropic-version"], ANTHROPIC_VERSION);
}

#[test]
fn federated_mode_never_sends_x_api_key() {
    let map = auth_headers(&federated_protocol(), NOOP_KEY, Some("minted")).unwrap();
    assert!(!map.contains_key("x-api-key"));
    assert_eq!(map["authorization"], "Bearer minted");
    assert_eq!(map["anthropic-beta"], OAUTH_BETA);
}

#[test]
fn the_noop_placeholder_is_never_sent_as_a_key() {
    let err = auth_headers(&Protocol::AnthropicApiKey, NOOP_KEY, None).unwrap_err();
    assert!(err.to_string().contains("no Anthropic API key"), "{err}");
}

#[test]
fn an_empty_api_key_is_rejected_before_any_request() {
    assert!(auth_headers(&Protocol::AnthropicApiKey, "", None).is_err());
}

#[test]
fn bearer_modes_require_a_token() {
    let missing = auth_headers(&Protocol::AnthropicOAuth, "sk-ant-real", None).unwrap_err();
    assert!(missing.to_string().contains("bearer token"), "{missing}");
    assert!(auth_headers(&Protocol::AnthropicOAuth, "x", Some("")).is_err());
    assert!(auth_headers(&federated_protocol(), "x", Some(NOOP_KEY)).is_err());
}

#[test]
fn openai_compat_is_not_an_anthropic_auth_mode() {
    assert!(auth_headers(&Protocol::OpenAiCompat, "sk-x", None).is_err());
}

#[test]
fn a_token_with_invalid_header_characters_is_rejected_without_leaking_it() {
    let err = auth_headers(&Protocol::AnthropicOAuth, NOOP_KEY, Some("bad\nvalue"))
        .expect_err("a newline cannot go in a header");
    let text = err.to_string();
    assert!(text.contains("invalid characters"), "{text}");
    assert!(
        !text.contains("bad"),
        "the error must not echo the credential: {text}"
    );
}

// --- error classification -----------------------------------------------------

/// Nothing reachable before a request goes out may surface as `Parse`, which
/// `turn.rs` renders as a malformed response and so blames the server for. The
/// credential cases classify as `auth` besides: a caller must never retry one.
#[test]
fn a_credential_rejected_before_the_wire_is_an_auth_failure() {
    let cases = [
        auth_headers(&Protocol::AnthropicApiKey, NOOP_KEY, None),
        auth_headers(&Protocol::AnthropicOAuth, NOOP_KEY, Some("bad\nvalue")),
    ];
    for case in cases {
        let err = case.expect_err("must fail before any request");
        assert!(matches!(err, ClientError::Auth(_)), "got {err:?}");
        assert_eq!(err.kind(), ErrorKind::Auth);
    }
}

#[test]
fn anthropic_headers_for_a_non_anthropic_source_are_a_bug_not_a_credential() {
    // No configuration produces this, so reporting it as an auth failure would
    // send someone hunting for a credential that was never the problem.
    let err = auth_headers(&Protocol::OpenAiCompat, "sk-x", None)
        .expect_err("must fail before any request");
    assert!(matches!(err, ClientError::Internal(_)), "got {err:?}");
    assert_eq!(err.kind(), ErrorKind::Internal);
}

#[tokio::test]
async fn a_bearer_for_a_non_bearer_source_is_an_internal_error() {
    let source = Source::new(
        "local",
        "http://localhost:8080/v1".to_string(),
        None,
        None,
        None,
        None,
    );
    let err = TokenCache::default()
        .bearer(&Client::new(), &source)
        .await
        .expect_err("an OpenAI-compatible source has no bearer to mint");
    assert!(matches!(err, ClientError::Internal(_)), "got {err:?}");
}

// --- exchange response parsing ------------------------------------------------

#[test]
fn minted_token_is_parsed_with_its_lifetime() {
    let (token, expires_at) =
        parse_minted(r#"{"access_token":"oat_123","expires_in":3600}"#).unwrap();
    assert_eq!(token, "oat_123");
    assert!(expires_at > Instant::now(), "should not be expired");
}

#[test]
fn a_short_lifetime_yields_an_already_stale_token_rather_than_panicking() {
    // expires_in below the skew must not underflow; it just re-mints next time.
    let (_, expires_at) = parse_minted(r#"{"access_token":"oat_123","expires_in":5}"#).unwrap();
    assert!(expires_at <= Instant::now());
}

#[test]
fn a_response_without_an_access_token_is_an_error() {
    let err = parse_minted(r#"{"expires_in":3600}"#).unwrap_err();
    assert!(err.to_string().contains("no access_token"), "{err}");
    assert!(parse_minted("not json").is_err());
    assert!(parse_minted(r#"{"access_token":""}"#).is_err());
}

#[test]
fn the_federation_beta_header_requests_both_betas() {
    // The exchange endpoint needs the oidc-federation beta in addition to oauth.
    assert!(FEDERATION_BETA.contains("oauth-2025-04-20"));
    assert!(FEDERATION_BETA.contains("oidc-federation-2026-04-01"));
}
