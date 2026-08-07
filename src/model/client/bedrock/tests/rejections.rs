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

/// The body is never classified, whatever the header says or does not. AWS
/// echoes the request into a validation message, so a conversation about
/// throttling would otherwise classify itself - and the wrong `Some` also
/// suppresses the tool hint, which only appears when nothing else explains the
/// rejection.
#[test]
fn a_body_echoing_the_prompt_is_never_classified() {
    let echoed = json!({
        "message": "Malformed input request: #/messages/0/content. Input started: \
                    'explain AWS throttling and access denied to me'",
    })
    .to_string();
    let error = rejection(&Rejection {
        model: "zai.glm-5",
        tools_sent: true,
        status: 400,
        error_type: None,
        body: echoed,
    });
    let message = error.to_string();
    assert!(!message.contains("throttled"), "got {message}");
    assert!(!message.contains("not entitled"), "got {message}");
    assert!(
        message.contains("cannot call tools"),
        "an unexplained rejection must keep its hint: {message}"
    );
}

/// A 403 with no header did not come from Bedrock's API layer - a proxy or a
/// VPC endpoint refusing on the way - so leading with an entitlement verdict
/// would send the operator to the Bedrock console for a network fault.
#[test]
fn a_headerless_403_is_not_called_an_entitlement_problem() {
    let error = rejection(&Rejection {
        model: "zai.glm-5",
        tools_sent: true,
        status: 403,
        error_type: None,
        body: "<html>403 Forbidden</html>".to_string(),
    });
    assert_eq!(error.to_string(), "HTTP 403: <html>403 Forbidden</html>");
}

/// A 429 needs no header: it means the same thing whoever sent it.
#[test]
fn a_headerless_429_is_still_a_throttle() {
    let error = rejection(&Rejection {
        model: "zai.glm-5",
        tools_sent: false,
        status: 429,
        error_type: None,
        body: String::new(),
    });
    assert_eq!(error.to_string(), "HTTP 429: AWS throttled the request");
}

/// The credentials are read once at startup, so re-selecting the source hands
/// back the same expired struct. The message has to say so.
#[test]
fn the_credential_message_names_the_restart() {
    let error = reject(
        403,
        "ExpiredTokenException",
        r#"{"message":"The security token included in the request is expired"}"#,
    );
    assert!(error.to_string().contains("needs a restart"), "got {error}");
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

/// The reference documents these strings verbatim, and the last time they moved
/// the docs were left behind. Asserted here so a message edit fails a test
/// rather than shipping a manual that describes the previous release.
///
/// Whitespace is collapsed on both sides, because the reference wraps the
/// example across lines inside its code block.
#[test]
fn the_reference_quotes_the_messages_this_module_produces() {
    fn flat(text: &str) -> String {
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }
    let doc = flat(include_str!("../../../../../docs/reference.md"));

    let hint = flat(&validation("This model does not support tool use.").to_string());
    let hint = hint.strip_prefix("HTTP 400: ").unwrap_or(&hint);
    assert!(
        doc.contains(hint),
        "the documented hint example is not the one produced: {hint}"
    );

    let expired = flat(&reject(403, "ExpiredTokenException", "").to_string());
    let expired = expired.strip_prefix("HTTP 403: ").unwrap_or(&expired);
    assert!(
        doc.contains(expired),
        "the documented credential row is not the message produced: {expired}"
    );

    assert!(
        !doc.contains("or any other 403"),
        "a headerless 403 is no longer an entitlement verdict, but the table still says so"
    );
}
