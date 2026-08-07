//! Amazon Bedrock on its `OpenAI`-compatible surface.
//!
//! Not a third wire protocol. Bedrock's `/v1/chat/completions` speaks the same
//! request and SSE shapes as [`super::openai`], so the body builders, the
//! decoder, and the turn loop are shared verbatim. Two things are Bedrock's own
//! and live here: the credential, which is an AWS `SigV4` signature computed per
//! request rather than a static header ([`sigv4`]), and the rejections, which
//! arrive as AWS exceptions and have to be told apart before they read as a
//! generic HTTP failure.
//!
//! Anthropic models on Bedrock are out of scope; a source pointed at one still
//! reaches Anthropic's own API through [`super::anthropic`].

use std::fmt::Write as _;

use chrono::Utc;
use reqwest::{Client, RequestBuilder, Url};
use serde_json::Value;

use super::{ClientError, into_header_map};
use crate::config::Bedrock;

pub(crate) mod sigv4;

/// The signing service name for `bedrock-runtime`. `bedrock` for both the
/// control plane and the runtime; only the host differs.
const SERVICE: &str = "bedrock";
const CONTENT_TYPE: &str = "application/json";

/// A POST carrying `body`, signed with `SigV4`.
///
/// The body is passed as the exact string that will be sent rather than as a
/// `Value`: the signature covers those bytes, so re-serializing downstream
/// would invalidate it.
pub(super) fn signed_post(
    http: &Client,
    bedrock: &Bedrock,
    source_name: &str,
    url: &str,
    body: String,
) -> Result<RequestBuilder, ClientError> {
    // `Runtime::refusals` already refuses a run whose starting source cannot
    // sign, but `/source` can switch to one mid-session, and that path has no
    // refusal gate. `Auth` rather than a transport failure: nothing was sent,
    // and no retry assembles the missing half of a credential.
    if let Some(refusal) = bedrock.incomplete(source_name) {
        return Err(ClientError::Auth(refusal));
    }
    // `Internal`, because a signature is scoped to a host and afi assembles this
    // url itself: the Region is charset-checked before it gets here and the path
    // is a literal. The one route an operator has to it is an
    // `AFI_BEDROCK_BASE_URL` that is not a url, which no protocol can send to.
    let parsed = Url::parse(url)
        .map_err(|e| ClientError::Internal(format!("bad Bedrock url {url:?}: {e}")))?;
    let host = host_header(&parsed).ok_or_else(|| {
        ClientError::Internal(format!("Bedrock url {url:?} names no host to sign for"))
    })?;
    // Every `unwrap_or_default` below is unreachable past the guard above.
    // Borrowed rather than unwrapped into owned copies, so the secret is never
    // duplicated out of the `Source` that holds it.
    let mut headers = vec![("content-type", CONTENT_TYPE.to_string())];
    headers.extend(sigv4::sign(
        &sigv4::CanonicalRequest {
            method: "POST",
            host: &host,
            path: parsed.path(),
            query: parsed.query().unwrap_or_default(),
            content_type: CONTENT_TYPE,
            body: body.as_bytes(),
            region: bedrock.region.as_deref().unwrap_or_default(),
            service: SERVICE,
            timestamp: &Utc::now().format("%Y%m%dT%H%M%SZ").to_string(),
        },
        &sigv4::Credentials {
            access_key_id: bedrock.access_key_id.as_deref().unwrap_or_default(),
            secret_access_key: bedrock.secret_access_key.as_deref().unwrap_or_default(),
            session_token: bedrock.session_token.as_deref(),
        },
    ));
    Ok(http.post(url).headers(into_header_map(headers)?).body(body))
}

/// `host:port`, with the port left off when it is the scheme's default - which
/// is what reqwest puts in the `Host` header, and so what has to be signed.
fn host_header(url: &Url) -> Option<String> {
    let host = url.host_str()?;
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    })
}

/// Everything known about a rejected Bedrock request.
pub(super) struct Rejection<'a> {
    pub model: &'a str,
    /// Whether the request offered tools. A rejection can only be about tool
    /// support when it did, which is what keeps the hint off `/compress`.
    pub tools_sent: bool,
    pub status: u16,
    /// `x-amzn-errortype`, where AWS names the exception. Absent on responses
    /// that never reached the service, such as a proxy's own error page.
    pub error_type: Option<String>,
    pub body: String,
}

/// Turn an AWS rejection into an error that says which kind it is.
///
/// Every one of these ends the run - the turn loop treats a client error as
/// terminal - so the message is the whole explanation the caller gets, and the
/// three that a Bedrock run actually hits are worth telling apart: credentials
/// that no longer work, a model the account cannot invoke, and throttling. AWS
/// names the exception in `x-amzn-errortype` and explains it in the body; the
/// classification comes from the first and the explanation is quoted from the
/// second, so the message AWS wrote is always the one reported.
///
/// **A model that cannot call tools is deliberately not a fourth kind.** AWS
/// returns `ValidationException` for that and for a malformed request alike -
/// same status, same header, nothing to tell them apart but the prose. Reading
/// that prose was tried and does not work: afi's own system prompt says "does
/// NOT support native tool calls", and any AWS error that quotes the request
/// back carries it, so the guess convicts the model of afi's own words. The
/// distinction also bought nothing, since a wrong tool schema and a
/// tool-incapable model are equally terminal here. What is left is the part the
/// wire does support: when a request that offered tools is rejected and nothing
/// else explains it, say what a missing tool capability would mean, as a
/// possibility rather than a finding.
pub(super) fn rejection(rejected: &Rejection<'_>) -> ClientError {
    let detail = aws_message(&rejected.body);
    let kind = classify(rejected);
    // 400 is where a capability rejection would arrive, and one AWS has already
    // explained as something else cannot also be about tools.
    let unexplained = kind.is_none() && rejected.status == 400 && rejected.tools_sent;
    // AWS returns a bodyless 4xx on some denials, so the separator is only
    // earned when there is something after it.
    let mut body = match kind {
        Some(kind) if detail.is_empty() => kind,
        Some(kind) => format!("{kind} - {detail}"),
        None => detail,
    };
    if unexplained {
        let _ = write!(
            body,
            " (if {} cannot call tools, an agent turn has nothing to dispatch)",
            rejected.model
        );
    }
    ClientError::Http {
        status: rejected.status,
        body,
    }
}

/// Which of the three failures a Bedrock run keeps hitting this is, or `None`
/// when nothing trustworthy says which.
///
/// Read from `x-amzn-errortype` and the status, never from the body. AWS echoes
/// the request into a validation message, so a conversation about throttling
/// would otherwise classify itself, and a wrong `Some` costs twice: it leads
/// with the wrong fix and it suppresses the tool hint, which appears only when
/// nothing else explains the rejection.
///
/// A response carrying no header did not come from Bedrock's own API layer - a
/// proxy or a VPC endpoint refusing on the way - and reading a kind out of its
/// error page is the same mistake. Those stay unclassified, so the body speaks
/// for itself rather than sending the operator to the Bedrock console for a
/// network fault.
fn classify(rejected: &Rejection<'_>) -> Option<String> {
    // Unambiguous whoever sent it, so it does not need the header.
    if rejected.status == 429 {
        return Some(THROTTLED.to_string());
    }
    let exception = rejected
        .error_type
        .as_deref()
        .filter(|kind| !kind.is_empty())?
        .to_lowercase();
    let names = |needles: &[&str]| needles.iter().any(|needle| exception.contains(needle));

    // Credentials first: an expired session token is a 403 like a denial is,
    // and the fix is the opposite one. The restart is named because the
    // credentials are read once at startup, so re-selecting the source with
    // `/source` hands back the same expired struct.
    if names(&[
        "expiredtoken",
        "invalidsignature",
        "unrecognizedclient",
        "invalidclienttokenid",
        "incompletesignature",
        "missingauthenticationtoken",
    ]) {
        return Some(
            "AWS rejected the credentials (expired or wrong; afi reads them at \
             startup, so a refresh needs a restart)"
                .to_string(),
        );
    }
    if names(&["throttling", "toomanyrequests", "servicequotaexceeded"]) {
        return Some(THROTTLED.to_string());
    }
    if names(&["accessdenied"]) {
        return Some(format!(
            "the account is not entitled to {} in this Region",
            rejected.model
        ));
    }
    None
}

const THROTTLED: &str = "AWS throttled the request";

/// What AWS said, unwrapped from whichever envelope it used. Falls back to the
/// raw body so nothing is ever swallowed.
fn aws_message(body: &str) -> String {
    let trimmed = body.trim();
    let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
        return trimmed.to_string();
    };
    for key in ["message", "Message", "errorMessage"] {
        if let Some(message) = value.get(key).and_then(Value::as_str) {
            return message.to_string();
        }
    }
    // The OpenAI-compatible surface wraps its own errors the OpenAI way.
    value
        .pointer("/error/message")
        .and_then(Value::as_str)
        .map_or_else(|| trimmed.to_string(), String::from)
}

#[cfg(test)]
mod tests;
