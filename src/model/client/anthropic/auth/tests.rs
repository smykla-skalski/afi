use super::*;
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
    assert!(matches!(err, ClientError::Auth(_)), "wrong variant");
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
    assert!(matches!(err, ClientError::Auth(_)), "got {err:?}");
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

// --- what the run summary reports ---------------------------------------------

/// A source on `protocol` holding a real credential.
fn keyed_source(protocol: Protocol) -> Source {
    Source::new(
        "test",
        "https://api.anthropic.com".to_string(),
        Some("sk-real-credential".to_string()),
        None,
        None,
        None,
    )
    .with_protocol(protocol)
}

#[test]
fn a_federated_run_reports_the_ids_the_grant_sent() {
    let source = keyed_source(federated_protocol());
    let auth = run_auth(&source);
    assert_eq!(auth.mode, MODE_FEDERATED);
    assert_eq!(auth.federation_rule_id, Some("fdrl_1"));
    assert_eq!(auth.organization_id, Some("org"));
    assert_eq!(auth.service_account_id, Some("svac"));
    // The fixture rule covers one workspace, so the exchange sends no
    // `workspace_id` and there is none to report.
    assert_eq!(auth.workspace_id, None);
}

#[test]
fn a_workspace_scoped_rule_reports_the_workspace_it_billed() {
    let mut federation = federation();
    federation.workspace_id = Some("wrkspc_ci".to_string());
    let source = keyed_source(Protocol::AnthropicFederated(Box::new(federation)));
    assert_eq!(run_auth(&source).workspace_id, Some("wrkspc_ci"));
}

/// The mode is minted, not stored, so the placeholder in `api_key` says nothing
/// about it. Reporting `none` here would deny a credential the run does have.
#[test]
fn a_federated_source_reports_federated_despite_holding_the_placeholder() {
    let source = Source::new(
        "anthropic",
        "https://api.anthropic.com".to_string(),
        None,
        None,
        None,
        None,
    )
    .with_protocol(federated_protocol());
    assert_eq!(source.api_key, NOOP_KEY, "the fixture must hold no key");
    assert_eq!(run_auth(&source).mode, MODE_FEDERATED);
}

#[test]
fn each_mode_is_named_by_how_the_credential_was_obtained() {
    // `OpenAiCompat` is an api key too: a static value out of the environment,
    // differing from `AnthropicApiKey` only in which header carries it.
    let api_key = keyed_source(Protocol::AnthropicApiKey);
    let openai = keyed_source(Protocol::OpenAiCompat);
    let oauth = keyed_source(Protocol::AnthropicOAuth);
    assert_eq!(run_auth(&api_key).mode, MODE_API_KEY);
    assert_eq!(run_auth(&openai).mode, MODE_API_KEY);
    assert_eq!(run_auth(&oauth).mode, MODE_OAUTH);
}

/// A keyless llama.cpp source must not claim a credential. `Source::new` stores
/// the placeholder, and `auth_headers` refuses to send it, so reporting
/// `api_key` would attest to something afi would not authenticate with.
#[test]
fn a_source_holding_the_placeholder_reports_no_credential() {
    for protocol in [Protocol::OpenAiCompat, Protocol::AnthropicApiKey] {
        let source = Source::new(
            "local",
            "http://localhost:8080/v1".to_string(),
            None,
            None,
            None,
            None,
        )
        .with_protocol(protocol.clone());
        assert_eq!(run_auth(&source).mode, MODE_NONE, "{protocol:?}");
    }
}

#[test]
fn a_static_credential_has_no_ids_to_report() {
    for protocol in [
        Protocol::AnthropicApiKey,
        Protocol::AnthropicOAuth,
        Protocol::OpenAiCompat,
    ] {
        let source = keyed_source(protocol.clone());
        let auth = run_auth(&source);
        let ids = [
            auth.organization_id,
            auth.service_account_id,
            auth.workspace_id,
            auth.federation_rule_id,
        ];
        assert!(ids.iter().all(Option::is_none), "{protocol:?}: {ids:?}");
    }
}

#[test]
fn the_identity_token_is_not_among_the_ids_reported() {
    // The summary is uploaded as an unmasked build artifact. The identity token
    // is a bearer credential in its own right, and it sits one field away from
    // the ids that are safe to publish.
    let source = keyed_source(Protocol::AnthropicFederated(Box::new(Federation {
        identity: Some(IdentitySource::Literal("eyJ.assertion".to_string())),
        ..federation()
    })));
    let auth = run_auth(&source);
    let reported = [
        auth.organization_id,
        auth.service_account_id,
        auth.workspace_id,
        auth.federation_rule_id,
    ];
    for id in reported.into_iter().flatten() {
        assert!(!id.contains("assertion"), "leaked the identity token: {id}");
    }
}

#[test]
fn the_federation_beta_header_requests_both_betas() {
    // The exchange endpoint needs the oidc-federation beta in addition to oauth.
    assert!(FEDERATION_BETA.contains("oauth-2025-04-20"));
    assert!(FEDERATION_BETA.contains("oidc-federation-2026-04-01"));
}

#[test]
fn a_refused_identity_exchange_is_an_auth_failure_not_a_transport_one() {
    // The one auth failure that arrives as an HTTP status. A federation rule that
    // turns the claims down - an unprotected ref, most often - answers 400 or 401,
    // and classifying that as the provider's trouble would make a caller retry
    // until the schedule ran out to be refused in the same words.
    for status in [StatusCode::BAD_REQUEST, StatusCode::UNAUTHORIZED] {
        let error = refused_credential("the exchange refused it", status, "{\"error\":\"x\"}");
        assert!(matches!(error, ClientError::Auth(_)), "got {error:?}");
        assert_eq!(error.kind(), ErrorKind::Auth);
    }
}

#[test]
fn a_busy_credential_endpoint_stays_retryable() {
    // Capacity, not the credential. This one is worth trying again, so it keeps
    // the status it arrived with.
    for status in [
        StatusCode::TOO_MANY_REQUESTS,
        StatusCode::SERVICE_UNAVAILABLE,
    ] {
        let error = refused_credential("busy", status, "slow down");
        assert_eq!(error.kind(), ErrorKind::ProviderHttp, "{status}");
    }
}

#[test]
fn a_refusal_body_is_quoted_but_bounded() {
    let error = refused_credential("refused", StatusCode::FORBIDDEN, &"x".repeat(1000));
    let text = error.to_string();
    assert!(text.contains("refused (HTTP 403)"), "{text}");
    assert!(
        text.matches('x').count() == BODY_PREVIEW_CHARS,
        "the body must be trimmed to the preview length: {}",
        text.len()
    );
}
