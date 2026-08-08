//! Reading the identity token, and how an endpoint that refuses a credential
//! reports one.

use std::path::PathBuf;

use reqwest::StatusCode;

use super::*;
use crate::config::{ANTHROPIC_IDENTITY, AWS_IDENTITY, IdentityVars};
use crate::summary::ErrorKind;

fn from(vars: IdentityVars, source: IdentitySource) -> Identity {
    Identity { vars, source }
}

// --- identity token -----------------------------------------------------------

#[tokio::test]
async fn a_blank_identity_token_file_names_the_file() {
    // Whitespace-only reads back as an empty assertion. Sending it earns a 400
    // about the grant, which says nothing about the file actually being empty.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("oidc");
    fs::write(&path, "   \n\t\n").unwrap();

    let err = fetch(
        &Client::new(),
        &from(ANTHROPIC_IDENTITY, IdentitySource::File(path.clone())),
    )
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
    let err = fetch(
        &Client::new(),
        &from(
            ANTHROPIC_IDENTITY,
            IdentitySource::Literal("  ".to_string()),
        ),
    )
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

    let token = fetch(
        &Client::new(),
        &from(ANTHROPIC_IDENTITY, IdentitySource::File(path)),
    )
    .await
    .unwrap();
    assert_eq!(token, "eyJhbGciOi.token");
}

/// The variable named in a refusal has to be the one the operator set, and the
/// two protocols spell it differently. Naming Anthropic's at an AWS source
/// would send them editing a variable nothing on that path reads.
#[tokio::test]
async fn a_missing_token_file_names_the_variable_of_its_own_protocol() {
    for (vars, expected) in [
        (ANTHROPIC_IDENTITY, "ANTHROPIC_IDENTITY_TOKEN_FILE"),
        (AWS_IDENTITY, "AWS_WEB_IDENTITY_TOKEN_FILE"),
    ] {
        let err = fetch(
            &Client::new(),
            &from(
                vars,
                IdentitySource::File(PathBuf::from("/nonexistent/oidc-token")),
            ),
        )
        .await
        .expect_err("a missing file is a config error");
        assert!(err.to_string().contains(expected), "{err}");
    }
}

/// The audience is not incidental: a token minted for one exchange is refused
/// by the other, with a rejection that names the claims rather than the
/// audience. It travels with the variables so the two cannot be paired wrongly.
#[test]
fn each_protocol_asks_for_its_own_audience() {
    assert_eq!(ANTHROPIC_IDENTITY.audience, "https://api.anthropic.com");
    assert_eq!(AWS_IDENTITY.audience, "sts.amazonaws.com");
}

#[test]
fn the_absent_token_message_names_both_variables_and_the_permission() {
    let message = Identity::absent(AWS_IDENTITY);
    assert!(message.contains("AWS_WEB_IDENTITY_TOKEN"), "{message}");
    assert!(message.contains("AWS_WEB_IDENTITY_TOKEN_FILE"), "{message}");
    assert!(message.contains("id-token: write"), "{message}");
}

// --- how a refused credential reports -----------------------------------------

#[test]
fn a_refused_identity_exchange_is_an_auth_failure_not_a_transport_one() {
    // The one auth failure that arrives as an HTTP status. A federation rule that
    // turns the claims down - an unprotected ref, most often - answers 400 or 401,
    // and classifying that as the provider's trouble would make a caller retry
    // until the schedule ran out to be refused in the same words.
    for status in [StatusCode::BAD_REQUEST, StatusCode::UNAUTHORIZED] {
        let error = refused_credential(
            "the exchange refused it",
            status,
            "{\"error\":\"x\"}",
            &Redactor::default(),
        );
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
        let error = refused_credential("busy", status, "slow down", &Redactor::default());
        assert_eq!(error.kind(), ErrorKind::ProviderHttp, "{status}");
    }
}

#[test]
fn a_retryable_refusal_still_names_the_step_that_failed() {
    // The retryable arm reports as a plain HTTP failure, which is the same shape
    // the model call's own rejections arrive in. Without the step, a 500 from a
    // credential exchange reads as the provider being down and sends whoever is
    // holding the run to a status page the request never reached.
    let error = refused_credential(
        "the AWS role assumption failed",
        StatusCode::INTERNAL_SERVER_ERROR,
        "<Error><Code>InternalFailure</Code></Error>",
        &Redactor::default(),
    );
    let text = error.to_string();
    assert_eq!(error.kind(), ErrorKind::ProviderHttp);
    assert!(text.contains("the AWS role assumption failed"), "{text}");
    assert!(text.contains("InternalFailure"), "AWS still speaks: {text}");
}

#[test]
fn a_refusal_body_is_quoted_but_bounded() {
    let error = refused_credential(
        "refused",
        StatusCode::FORBIDDEN,
        &"x".repeat(1000),
        &Redactor::default(),
    );
    let text = error.to_string();
    assert!(text.contains("refused (HTTP 403)"), "{text}");
    assert!(
        text.matches('x').count() == BODY_PREVIEW_CHARS,
        "the body must be trimmed to the preview length: {}",
        text.len()
    );
}

// --- credentials in the reported body -----------------------------------------

/// Stands in for the OIDC assertion an exchange is posted.
const ASSERTION: &str = "eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiJyZXBvOmFjbWUvYWZpIn0.signature";

/// A refusal that quotes the request it turned down, credential and all.
fn echoed_refusal(assertion: &str) -> String {
    format!(
        r#"{{"error":{{"type":"invalid_request_error","message":"unprotected ref"}},"request":{{"assertion":"{assertion}"}}}}"#
    )
}

/// One refusal of the assertion, as an exchange reports it.
fn refused(status: StatusCode, body: &str) -> ClientError {
    let redact = Redactor::default().with(ASSERTION, Credential::IdentityToken);
    refused_credential("the exchange refused it", status, body, &redact)
}

#[test]
fn a_refused_exchange_does_not_report_the_assertion_it_posted() {
    // The endpoint echoes the request it turned down, so the body it returns
    // holds the credential afi just sent. That sentence goes to stderr and to the
    // run summary, and afi fetched the token outside the toolkit that would have
    // masked it, so nothing further down catches it.
    let text = refused(StatusCode::BAD_REQUEST, &echoed_refusal(ASSERTION)).to_string();
    assert!(!text.contains(ASSERTION), "{text}");
    assert!(text.contains("[redacted OIDC identity token]"), "{text}");
}

#[test]
fn a_refusal_still_says_why_it_was_refused() {
    // A rejected credential has to stay distinguishable from a rate limit, so
    // only the credential goes.
    let text = refused(StatusCode::BAD_REQUEST, &echoed_refusal(ASSERTION)).to_string();
    assert!(text.contains("invalid_request_error"), "{text}");
    assert!(text.contains("unprotected ref"), "{text}");
}

#[test]
fn the_preview_cannot_reveal_what_redaction_removed() {
    // The 200-character window is applied after cleaning. Cutting first would
    // leave whichever half of the credential fell inside it.
    let padding = "p".repeat(BODY_PREVIEW_CHARS * 2);
    let text = refused(StatusCode::UNAUTHORIZED, &format!("{ASSERTION}{padding}")).to_string();
    assert!(!text.contains(&ASSERTION[..20]), "{text}");
}

#[test]
fn a_busy_endpoint_reports_a_clean_body_too() {
    // The retryable branch keeps its status and its whole body, which is exactly
    // the body the echo was in.
    let error = refused(StatusCode::SERVICE_UNAVAILABLE, &echoed_refusal(ASSERTION));
    let text = error.to_string();
    assert_eq!(error.kind(), ErrorKind::ProviderHttp);
    assert!(!text.contains(ASSERTION), "{text}");
}
