//! Signing and the credential gate. How a rejection reads lives in
//! [`rejections`], split out only to keep both files under the repository's
//! per-file line cap.

use super::{host_header, signed_post};
use crate::config::Bedrock;
use crate::model::client::ClientError;

fn complete() -> Bedrock {
    Bedrock {
        region: Some("us-east-1".to_string()),
        access_key_id: Some("AKIDEXAMPLE".to_string()),
        secret_access_key: Some("wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_string()),
        session_token: None,
    }
}

mod rejections;

// --- signing ----------------------------------------------------------------

#[test]
fn a_signed_post_carries_the_sigv4_headers_and_no_bearer() {
    let request = signed_post(
        &reqwest::Client::new(),
        &complete(),
        "bedrock",
        "https://bedrock-runtime.us-east-1.amazonaws.com/v1/chat/completions",
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
    let mut bedrock = complete();
    bedrock.session_token = Some("session-token".to_string());
    let request = signed_post(
        &reqwest::Client::new(),
        &bedrock,
        "bedrock",
        "https://bedrock-runtime.us-east-1.amazonaws.com/v1/chat/completions",
        "{}".to_string(),
    )
    .expect("a complete credential signs")
    .build()
    .expect("the signed request builds");
    assert_eq!(request.headers()["x-amz-security-token"], "session-token");
}

/// The refusal `Runtime::refusals` raises before the run also has to hold for a
/// mid-session `/source` switch, which has no refusal gate.
#[test]
fn an_incomplete_credential_is_refused_before_anything_is_sent() {
    let mut bedrock = complete();
    bedrock.secret_access_key = None;
    let error = signed_post(
        &reqwest::Client::new(),
        &bedrock,
        "bedrock",
        "https://bedrock-runtime.us-east-1.amazonaws.com/v1/chat/completions",
        "{}".to_string(),
    )
    .expect_err("an unsignable request must not be sent");
    let ClientError::Auth(message) = error else {
        panic!("a missing credential is an auth failure, not a transport one");
    };
    assert!(message.contains("AWS_SECRET_ACCESS_KEY"), "got {message}");
    assert!(message.contains("bedrock"), "got {message}");
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
