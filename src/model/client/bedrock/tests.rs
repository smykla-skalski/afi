//! Signing, where the credential comes from, and the gate in front of both. How
//! a rejection reads lives in [`rejections`], split out only to keep both files
//! under the repository's per-file line cap.

use super::{CredentialCache, Signing, host_header, redactor, signed_post, signing};
use crate::config::{
    AWS_IDENTITY, Bedrock, Identity, IdentitySource, Protocol, Source, WebIdentity,
};
use crate::model::client::redact::Credential;
use crate::model::client::{ClientError, Redactor};

const URL: &str = "https://bedrock-runtime.us-east-1.amazonaws.com/v1/chat/completions";

fn complete() -> Bedrock {
    Bedrock {
        region: Some("us-east-1".to_string()),
        access_key_id: Some("AKIDEXAMPLE".to_string()),
        secret_access_key: Some("wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_string()),
        session_token: None,
        web_identity: None,
    }
}

/// The static credential above, resolved into what a request signs with.
fn static_signing() -> Signing {
    Signing {
        region: "us-east-1".to_string(),
        access_key_id: "AKIDEXAMPLE".to_string(),
        secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_string(),
        session_token: None,
    }
}

/// A source on `bedrock`, as `discover_sources` would build one.
fn source(bedrock: Bedrock) -> Source {
    Source::new(
        "bedrock",
        "https://bedrock-runtime.us-east-1.amazonaws.com/v1".to_string(),
        None,
        None,
        None,
        None,
    )
    .with_protocol(Protocol::Bedrock(Box::new(bedrock)))
}

/// A role assumption that is complete enough to be attempted.
fn web_identity() -> WebIdentity {
    WebIdentity {
        role_arn: "arn:aws:iam::123456789012:role/afi-ci".to_string(),
        session_name: "afi".to_string(),
        identity: Some(Identity {
            vars: AWS_IDENTITY,
            source: IdentitySource::Literal("eyJhbGciOi.assertion".to_string()),
        }),
    }
}

mod rejections;

// --- signing ----------------------------------------------------------------

#[test]
fn a_signed_post_carries_the_sigv4_headers_and_no_bearer() {
    let request = signed_post(
        &reqwest::Client::new(),
        &static_signing(),
        URL,
        r#"{"model":"zai.glm-5"}"#.to_string(),
    )
    .expect("a complete credential signs")
    .build()
    .expect("the signed request builds");
    let headers = request.headers();
    assert!(
        headers["authorization"]
            .to_str()
            .unwrap()
            .starts_with("AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/"),
    );
    assert_eq!(headers["content-type"], "application/json");
    assert!(headers.contains_key("x-amz-date"));
    assert!(
        !headers.contains_key("x-amz-security-token"),
        "a long-lived key has no session token to send"
    );
}

#[test]
fn a_session_token_reaches_the_wire() {
    let mut signing = static_signing();
    signing.session_token = Some("session-token".to_string());
    let request = signed_post(&reqwest::Client::new(), &signing, URL, "{}".to_string())
        .expect("a complete credential signs")
        .build()
        .expect("the signed request builds");
    assert_eq!(request.headers()["x-amz-security-token"], "session-token");
}

// --- where the credential comes from -----------------------------------------

#[tokio::test]
async fn a_static_credential_is_used_as_it_stands() {
    let signing = signing(
        &reqwest::Client::new(),
        &CredentialCache::default(),
        &source(complete()),
    )
    .await
    .expect("a complete credential resolves")
    .expect("a Bedrock source signs");
    assert_eq!(signing.access_key_id, "AKIDEXAMPLE");
    assert_eq!(signing.region, "us-east-1");
}

/// Every other protocol carries its credential in a header, so there is nothing
/// to resolve and nothing to sign - and `authed_post` reads exactly this to
/// decide which of the two a request is.
#[tokio::test]
async fn a_source_on_another_protocol_signs_nothing() {
    let source = Source::new(
        "local",
        "http://localhost:8080/v1".to_string(),
        Some("sk-real".to_string()),
        None,
        None,
        None,
    );
    let signing = signing(
        &reqwest::Client::new(),
        &CredentialCache::default(),
        &source,
    )
    .await
    .expect("no credential to resolve");
    assert!(signing.is_none());
}

/// The refusal `Runtime::refusals` raises before the run also has to hold for a
/// mid-session `/source` switch, which has no refusal gate.
#[tokio::test]
async fn an_incomplete_credential_is_refused_before_anything_is_sent() {
    let mut bedrock = complete();
    bedrock.secret_access_key = None;
    let error = signing(
        &reqwest::Client::new(),
        &CredentialCache::default(),
        &source(bedrock),
    )
    .await
    .expect_err("an unsignable request must not be sent");
    let ClientError::Auth(message) = error else {
        panic!("a missing credential is an auth failure, not a transport one");
    };
    assert!(message.contains("AWS_SECRET_ACCESS_KEY"), "got {message}");
    assert!(message.contains("bedrock"), "got {message}");
}

/// A role whose ARN is not one never reaches STS: the refusal names the
/// variable rather than quoting a `ValidationError` about a request nobody
/// wrote.
#[tokio::test]
async fn a_role_that_is_not_an_arn_is_refused_before_the_exchange() {
    let mut bedrock = complete();
    bedrock.access_key_id = None;
    bedrock.secret_access_key = None;
    bedrock.web_identity = Some(WebIdentity {
        role_arn: "afi-ci".to_string(),
        ..web_identity()
    });
    let error = signing(
        &reqwest::Client::new(),
        &CredentialCache::default(),
        &source(bedrock),
    )
    .await
    .expect_err("a role name is not a role ARN");
    assert!(error.to_string().contains("AWS_ROLE_ARN"), "got {error}");
}

/// A source holding both resolves the static pair here, with no exchange - so
/// nothing reaches STS on a path that never needed it. Which of the two wins is
/// `Bedrock::federating`'s to decide and is tested there.
#[tokio::test]
async fn a_static_pair_is_resolved_without_assuming_the_role() {
    let mut bedrock = complete();
    bedrock.web_identity = Some(web_identity());
    let signing = signing(
        &reqwest::Client::new(),
        &CredentialCache::default(),
        &source(bedrock),
    )
    .await
    .expect("the static pair resolves without any exchange")
    .expect("a Bedrock source signs");
    assert_eq!(signing.access_key_id, "AKIDEXAMPLE");
}

// --- credentials in a reported body ------------------------------------------

/// The session token rides `x-amz-security-token` on every signed request, so a
/// gateway that echoes the request it refused hands it straight back - and on
/// the federated path afi minted it itself, where nothing upstream masks it.
#[test]
fn a_reported_body_does_not_carry_the_session_token_the_request_sent() {
    let mut signing = static_signing();
    signing.session_token = Some("FwoGZXIvYXdzEExample//////".to_string());
    let cleaned = redactor(&source(complete()), Some(&signing))
        .clean("AccessDeniedException: x-amz-security-token=FwoGZXIvYXdzEExample//////");
    assert!(!cleaned.contains("FwoGZXIvYXdzEExample"), "{cleaned}");
    assert!(
        cleaned.contains("[redacted AWS session token]"),
        "{cleaned}"
    );
    assert!(
        cleaned.contains("AccessDeniedException"),
        "the reason survives: {cleaned}"
    );
}

#[test]
fn a_source_with_no_session_token_leaves_its_body_alone() {
    let body = "AccessDeniedException: not entitled";
    assert_eq!(
        redactor(&source(complete()), Some(&static_signing())).clean(body),
        body
    );
    assert_eq!(redactor(&source(complete()), None).clean(body), body);
    // The marker exists so a struck credential reads differently from a body
    // that merely ran long.
    assert_eq!(
        Redactor::default()
            .with("a-long-session-token", Credential::SessionToken)
            .clean("token=a-long-session-token"),
        "token=[redacted AWS session token]"
    );
}

#[test]
fn host_header_keeps_a_non_default_port_and_drops_a_default_one() {
    let host = |url: &str| host_header(&reqwest::Url::parse(url).unwrap());
    assert_eq!(
        host("https://bedrock-runtime.us-east-1.amazonaws.com/v1"),
        Some("bedrock-runtime.us-east-1.amazonaws.com".to_string())
    );
    assert_eq!(
        host("https://gateway.internal:8443/v1"),
        Some("gateway.internal:8443".to_string())
    );
    assert_eq!(
        host("https://gateway.internal:443/v1"),
        Some("gateway.internal".to_string()),
        "reqwest omits the default port from Host, so the signature must too"
    );
}
