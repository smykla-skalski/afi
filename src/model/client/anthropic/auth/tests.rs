use super::*;

fn federated_protocol() -> Protocol {
    Protocol::AnthropicFederated(Box::new(Federation {
        rule_id: "fdrl_1".to_string(),
        organization_id: "org".to_string(),
        service_account_id: "svac".to_string(),
        workspace_id: None,
        identity: None,
    }))
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

/// Every failure reachable before a request goes out is the user's own config,
/// so none of them may surface as `Parse`. `turn.rs` renders `Parse` as a
/// malformed-response problem, which points the blame at the server.
#[test]
fn failures_before_the_wire_are_config_errors() {
    let cases = [
        auth_headers(&Protocol::OpenAiCompat, "sk-x", None),
        auth_headers(&Protocol::AnthropicApiKey, NOOP_KEY, None),
        auth_headers(&Protocol::AnthropicOAuth, NOOP_KEY, Some("bad\nvalue")),
    ];
    for case in cases {
        let err = case.expect_err("must fail before any request");
        assert!(matches!(err, ClientError::Config(_)), "got {err:?}");
    }
}

#[tokio::test]
async fn a_bearer_for_a_non_bearer_source_is_a_config_error() {
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
    assert!(matches!(err, ClientError::Config(_)), "got {err:?}");
}

// --- identity token -----------------------------------------------------------

#[tokio::test]
async fn a_blank_identity_token_file_names_the_file() {
    // Whitespace-only reads back as an empty assertion. Sending it earns a 400
    // about the grant, which says nothing about the file actually being empty.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("oidc");
    fs::write(&path, "   \n\t\n").unwrap();

    let err = fetch_identity_token(&Client::new(), &IdentitySource::File(path.clone()))
        .await
        .expect_err("an empty token file must not reach the exchange");
    let text = err.to_string();
    assert!(matches!(err, ClientError::Config(_)), "wrong variant");
    assert!(text.contains("is empty"), "{text}");
    assert!(text.contains(&path.display().to_string()), "{text}");
}

#[tokio::test]
async fn a_blank_literal_identity_token_is_rejected() {
    // `IdentitySource::from_env` filters empties, so this guards the type itself
    // rather than that one path.
    let err = fetch_identity_token(&Client::new(), &IdentitySource::Literal("  ".to_string()))
        .await
        .expect_err("a blank literal token must not reach the exchange");
    assert!(matches!(err, ClientError::Config(_)), "got {err:?}");
}

#[tokio::test]
async fn a_token_file_is_read_and_trimmed() {
    // A file written by a shell redirect almost always ends in a newline, which
    // would be rejected as an invalid header character downstream.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("oidc");
    fs::write(&path, "  eyJhbGciOi.token\n").unwrap();

    let token = fetch_identity_token(&Client::new(), &IdentitySource::File(path))
        .await
        .unwrap();
    assert_eq!(token, "eyJhbGciOi.token");
}

#[tokio::test]
async fn a_missing_token_file_names_the_variable() {
    let err = fetch_identity_token(
        &Client::new(),
        &IdentitySource::File("/nonexistent/oidc-token".into()),
    )
    .await
    .expect_err("a missing file is a config error");
    assert!(
        err.to_string().contains("ANTHROPIC_IDENTITY_TOKEN_FILE"),
        "{err}"
    );
}

// --- exchange response parsing ------------------------------------------------

#[test]
fn minted_token_is_parsed_with_its_lifetime() {
    let token = parse_minted(r#"{"access_token":"oat_123","expires_in":3600}"#).unwrap();
    assert_eq!(token.value, "oat_123");
    assert!(token.expires_at > Instant::now(), "should not be expired");
}

#[test]
fn a_short_lifetime_yields_an_already_stale_token_rather_than_panicking() {
    // expires_in below the skew must not underflow; it just re-mints next time.
    let token = parse_minted(r#"{"access_token":"oat_123","expires_in":5}"#).unwrap();
    assert!(token.expires_at <= Instant::now());
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
