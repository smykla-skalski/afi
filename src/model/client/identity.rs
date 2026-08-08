//! The OIDC identity token both federated paths start from, and the rule for
//! reporting an endpoint that refuses one.
//!
//! Anthropic's token exchange and AWS's role assumption agree on nothing
//! downstream - one posts a JWT-bearer grant and gets a bearer token back, the
//! other posts a query action and gets a key pair - and agree exactly here: a
//! JWT, read from a variable, from a file, or minted by GitHub Actions for the
//! audience that exchange requires. Shared so a workflow granting
//! `id-token: write` reaches either one with the same setup, and so the rule
//! that a refused credential is never echoed back has one implementation
//! instead of one per protocol.

use std::fs;
use std::time::Duration;

use reqwest::{Client, StatusCode, Url};
use serde_json::Value;

use crate::config::{Identity, IdentitySource};

use super::{
    BODY_PREVIEW_CHARS, ClientError, Credential, Redactor, transport_error, transport_error_at,
};

/// Fetch the OIDC identity token, rejecting a blank one.
///
/// A blank token is a local misconfiguration - an empty or whitespace-only
/// token file, or an Actions response carrying an empty `value`. Left alone it
/// would reach the exchange as an empty assertion, and what comes back names
/// the grant rather than the empty file, hiding the real cause.
pub(super) async fn fetch(http: &Client, identity: &Identity) -> Result<String, ClientError> {
    let token = read(http, identity).await?;
    let token = token.trim();
    if token.is_empty() {
        return Err(ClientError::Auth(format!(
            "the OIDC identity token from {} is empty",
            identity.label()
        )));
    }
    Ok(token.to_string())
}

/// Read the raw token. Never logged.
async fn read(http: &Client, identity: &Identity) -> Result<String, ClientError> {
    match &identity.source {
        IdentitySource::Literal(token) => Ok(token.clone()),
        IdentitySource::File(path) => fs::read_to_string(path).map_err(|e| {
            ClientError::Auth(format!(
                "cannot read {} {}: {e}",
                identity.vars.file,
                path.display()
            ))
        }),
        IdentitySource::GithubActions { url, request_token } => {
            github_token(http, url, request_token, identity.vars.audience).await
        }
    }
}

/// Mint a token at the Actions OIDC endpoint, for the audience the exchange
/// that will receive it requires.
///
/// The audience is not incidental. A token minted for one exchange is refused
/// by the other, and the refusal comes back as an opaque rejection of the
/// claims rather than as "wrong audience" - so it travels with the variables
/// the token was resolved under rather than being chosen here.
async fn github_token(
    http: &Client,
    url: &str,
    request_token: &str,
    audience: &str,
) -> Result<String, ClientError> {
    // The Actions runtime url already carries an `api-version` query, so the
    // audience is appended rather than replacing the query string.
    let mut endpoint = Url::parse(url)
        // Supplied by the Actions runtime, so a malformed one is environmental.
        .map_err(|e| ClientError::Auth(format!("bad ACTIONS_ID_TOKEN_REQUEST_URL: {e}")))?;
    endpoint.query_pairs_mut().append_pair("audience", audience);
    let response = http
        .get(endpoint)
        .header("authorization", format!("Bearer {request_token}"))
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| transport_error_at(Some("OIDC token request failed"), &e))?;
    let status = response.status();
    let body = response.text().await.map_err(|e| transport_error(&e))?;
    if !status.is_success() {
        // Normally an Actions error rather than a credential - a 403 is the
        // workflow lacking `permissions: id-token: write` - but the runtime token
        // went out in the request header, so an endpoint or proxy that echoes the
        // request hands it straight back.
        return Err(refused_credential(
            "the GitHub Actions OIDC endpoint refused the token request",
            status,
            &body,
            &Redactor::default().with(request_token, Credential::RequestToken),
        ));
    }
    json_string_field(&body, "value")
        .ok_or_else(|| ClientError::Parse("OIDC response had no `value` field".to_string()))
}

/// A failing status from an endpoint that hands out credentials.
///
/// A 4xx here is the credential itself being turned down - a 401 from a token
/// exchange almost always means the OIDC claims did not satisfy the rule they
/// were checked against, most often an unprotected ref - so it reports as an
/// auth failure rather than as the transport error it arrives as. Retrying one
/// spends the schedule to be refused in the same words. A 429 or a 5xx is the
/// endpoint having a bad day and keeps its status, because that one is worth
/// another attempt.
///
/// `what` leads either way. Both of these arrive mid-request, alongside the
/// statuses the model call itself returns, and a bare `HTTP 500` says nothing
/// about which of the two failed - where the step is the whole difference
/// between a credential exchange worth repeating and a refusal the operator was
/// waiting on. The transport failures at the same call sites already name their
/// step; this is the same sentence on the path that got an answer.
///
/// `redact` is taken rather than left to callers because every endpoint here is
/// handed the credential it is being asked about, and every one of them quotes
/// what comes back. Cleaning runs before the preview is cut, so the window can
/// only ever trim a marker.
pub(super) fn refused_credential(
    what: &str,
    status: StatusCode,
    body: &str,
    redact: &Redactor,
) -> ClientError {
    let body = redact.clean(body);
    if status.is_client_error() && status != StatusCode::TOO_MANY_REQUESTS {
        let preview: String = body.chars().take(BODY_PREVIEW_CHARS).collect();
        return ClientError::Auth(format!("{what} (HTTP {}): {preview}", status.as_u16()));
    }
    ClientError::Http {
        status: status.as_u16(),
        body: format!("{what}: {body}"),
    }
}

/// A non-empty string field of a JSON body, or `None`.
pub(super) fn json_string_field(body: &str, key: &str) -> Option<String> {
    serde_json::from_str::<Value>(body)
        .ok()?
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

#[cfg(test)]
mod tests;
