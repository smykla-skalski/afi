//! How an AWS rejection reads: which failure it is told to be, and what
//! separates a verdict on the model from a bug in the request.

use serde_json::json;

use super::super::{Rejection, rejection};
use crate::model::client::ClientError;

/// A rejection of a tools-bearing request, which is what an agent turn sends.
fn reject(status: u16, error_type: &str, body: &str) -> ClientError {
    rejection(&Rejection {
        model: "zai.glm-5",
        tools_sent: true,
        status,
        error_type: Some(error_type.to_string()),
        body: body.to_string(),
    })
}

/// The 400 AWS returns for anything it considers a bad request, carrying
/// `message` in the envelope it always uses. Most cases below differ only in
/// that string.
fn validation(message: &str) -> ClientError {
    reject(
        400,
        "ValidationException",
        &json!({ "message": message }).to_string(),
    )
}

#[test]
fn expired_credentials_are_told_from_a_denial() {
    let error = reject(
        403,
        "ExpiredTokenException",
        r#"{"message":"The security token included in the request is expired"}"#,
    );
    let message = error.to_string();
    assert!(
        message.contains("rejected the credentials"),
        "got {message}"
    );
    assert!(
        message.contains("The security token included in the request is expired"),
        "AWS's own wording has to survive: {message}"
    );
}

#[test]
fn an_unentitled_model_names_the_model() {
    let error = reject(
        403,
        "AccessDeniedException",
        r#"{"message":"You don't have access to the model with the specified model ID."}"#,
    );
    let message = error.to_string();
    assert!(
        message.contains("not entitled to zai.glm-5"),
        "got {message}"
    );
    assert!(message.contains("You don't have access"), "got {message}");
}

#[test]
fn throttling_is_told_apart_from_both() {
    let error = reject(
        429,
        "ThrottlingException",
        r#"{"message":"Too many requests, please wait before trying again."}"#,
    );
    let message = error.to_string();
    assert!(message.contains("throttled"), "got {message}");
    assert!(message.contains("Too many requests"), "got {message}");
}

/// An invalid signature is a 403 like a denial, and the two need opposite
/// fixes, so the credential check has to win.
#[test]
fn a_bad_signature_reads_as_a_credential_problem_not_a_denial() {
    let error = reject(
        403,
        "InvalidSignatureException",
        r#"{"message":"Signature expired: 20260807T050217Z is now earlier than ..."}"#,
    );
    assert!(
        error.to_string().contains("rejected the credentials"),
        "got {error}"
    );
}

#[test]
fn an_unclassified_rejection_still_reports_what_aws_said() {
    let error = validation("Malformed input request: #/messages: expected minimum item count: 1");
    let message = error.to_string();
    assert!(
        message.starts_with("HTTP 400: Malformed input request"),
        "got {message}"
    );
}

/// AWS echoes the request back in a validation message, so a prompt that talks
/// about throttling must not outvote the exception AWS named in the header.
#[test]
fn a_body_echoing_the_prompt_does_not_outvote_the_error_type() {
    let echoed = json!({
        "message": "Malformed input request: #/messages/0/content: expected type \
                    string. Input started: 'explain AWS throttling and access denied'",
    })
    .to_string();
    let error = reject(400, "ValidationException", &echoed);
    let message = error.to_string();
    assert!(!message.contains("throttled"), "got {message}");
    assert!(!message.contains("not entitled"), "got {message}");
    assert!(
        message.starts_with("HTTP 400: Malformed input"),
        "got {message}"
    );
}

/// With no `x-amzn-errortype` there is nothing but the body to go on, so it is
/// still searched - a gateway in front of Bedrock may drop the header.
#[test]
fn the_body_is_still_classified_when_aws_named_no_error_type() {
    let error = rejection(&Rejection {
        model: "zai.glm-5",
        tools_sent: true,
        status: 400,
        error_type: None,
        body: r#"{"message":"ThrottlingException: rate exceeded"}"#.to_string(),
    });
    assert!(error.to_string().contains("throttled"), "got {error}");
}

/// AWS returns a bodyless 4xx on some denials.
#[test]
fn an_empty_body_leaves_no_dangling_separator() {
    assert_eq!(
        reject(403, "AccessDeniedException", "").to_string(),
        "HTTP 403: the account is not entitled to zai.glm-5 in this Region"
    );
}

#[test]
fn an_error_body_that_is_not_json_is_reported_verbatim() {
    let error = reject(502, "", "<html>Bad Gateway</html>");
    assert_eq!(error.to_string(), "HTTP 502: <html>Bad Gateway</html>");
}

#[test]
fn an_openai_shaped_error_envelope_is_unwrapped_too() {
    let error = reject(400, "", r#"{"error":{"message":"model not found"}}"#);
    assert!(
        error.to_string().starts_with("HTTP 400: model not found"),
        "got {error}"
    );
}

// --- the tool-capability hint -------------------------------------------------

/// A rejection AWS did not otherwise explain may be the model saying it cannot
/// call tools. afi cannot tell that from a malformed request - AWS uses
/// `ValidationException` for both - so it says what a missing capability would
/// mean without claiming that is what happened.
#[test]
fn an_unexplained_rejection_of_a_tools_request_names_the_possibility() {
    let error = validation("This model does not support tool use.");
    let message = error.to_string();
    assert!(
        message.starts_with("HTTP 400: This model does not support tool use."),
        "AWS's own sentence leads: {message}"
    );
    assert!(
        message.contains("if zai.glm-5 cannot call tools"),
        "the hint is offered as a possibility: {message}"
    );
    assert!(
        matches!(error, ClientError::Http { .. }),
        "no verdict is claimed, so this is an ordinary HTTP failure"
    );
}

/// The hint rides along on a malformed-request 400 too, which is the price of
/// not guessing. It stays a hint, and AWS's message still leads.
#[test]
fn the_hint_does_not_displace_what_aws_said() {
    let message = validation(
        "Malformed input request: #/tools/0/function/parameters: \
                              unsupported keyword [$schema]",
    )
    .to_string();
    assert!(
        message.starts_with("HTTP 400: Malformed input request: #/tools/0"),
        "got {message}"
    );
    assert!(
        message.contains("if zai.glm-5 cannot call tools"),
        "got {message}"
    );
}

/// `/compress` sends no tools, so nothing there can be about tool support.
#[test]
fn a_request_without_tools_gets_no_hint() {
    let error = rejection(&Rejection {
        model: "zai.glm-5",
        tools_sent: false,
        status: 400,
        error_type: Some("ValidationException".to_string()),
        body: r#"{"message":"Malformed input request"}"#.to_string(),
    });
    assert_eq!(error.to_string(), "HTTP 400: Malformed input request");
}

/// A rejection AWS *has* explained is not also a maybe-about-tools one.
#[test]
fn an_explained_rejection_gets_no_hint() {
    for (status, error_type) in [
        (403, "AccessDeniedException"),
        (429, "ThrottlingException"),
        (403, "ExpiredTokenException"),
    ] {
        let error = reject(status, error_type, r#"{"message":"nope"}"#);
        assert!(
            !error.to_string().contains("cannot call tools"),
            "{error_type} is already explained: {error}"
        );
    }
}
