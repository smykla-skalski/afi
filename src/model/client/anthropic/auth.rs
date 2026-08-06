//! Anthropic authentication: header construction and the workload-identity
//! federation token exchange.
//!
//! # The `x-api-key` trap
//!
//! The Messages API validates `x-api-key` *in preference to* a bearer token. A
//! non-empty but wrong `x-api-key` fails the request even when a perfectly good
//! `Authorization: Bearer` is present; an empty one is ignored. Since
//! `Source::new` stores [`NOOP_KEY`] whenever no key was configured, the bearer
//! modes must omit the header entirely rather than send a placeholder.

use std::collections::HashMap;
use std::fs;
use std::time::{Duration, Instant};

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Client, StatusCode, Url};
use serde_json::Value;
use tokio::sync::RwLock;

use crate::config::{Federation, IdentitySource, NOOP_KEY, Protocol, Source};
use crate::model::client::{BODY_PREVIEW_CHARS, ClientError, transport_error, transport_error_at};
use crate::summary::RunAuth;

/// Pinned API version. Anthropic requires this on every request.
const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Required on `/v1/messages` whenever auth is a bearer token rather than a key.
const OAUTH_BETA: &str = "oauth-2025-04-20";
/// Additionally required on the token-exchange endpoint.
const FEDERATION_BETA: &str = "oauth-2025-04-20,oidc-federation-2026-04-01";
/// Audience the OIDC identity token must be minted for.
const OIDC_AUDIENCE: &str = "https://api.anthropic.com";
/// Re-mint this long before expiry so a request never races the deadline.
const EXPIRY_SKEW: Duration = Duration::from_mins(1);

/// A static key out of the environment, whichever header carries it.
const MODE_API_KEY: &str = "api_key";
/// A bearer token minted elsewhere and handed to afi.
const MODE_OAUTH: &str = "oauth";
/// A bearer token afi minted itself, through [`exchange`].
const MODE_FEDERATED: &str = "federated";
/// No credential was configured at all - a local server that wants none.
const MODE_NONE: &str = "none";

/// Describe the credential this source authenticates with, for the run summary.
///
/// Built here rather than in `summary.rs` because the identifiers it reports are
/// exactly the non-secret ones [`exchange`] puts in the grant: an id that stops
/// being part of the exchange stops being part of the report. The assertion and
/// the minted token are not among them and never may be - see [`RunAuth`].
///
/// The ids are what the exchange *sent*, not what the response was scoped to.
/// Anthropic returns a token and nothing to attribute it with, so a rule that
/// resolves a single workspace server-side leaves `workspace_id` absent here.
///
/// A source holding the placeholder reports `none` rather than `api_key`. It is
/// the llama.cpp case: `Source::new` stores [`NOOP_KEY`] when nothing was
/// configured, so a keyless local server would otherwise claim a credential it
/// never had. `OpenAiCompat` with a real key does report `api_key` - a static
/// key is a static key, and only the header differs from `AnthropicApiKey`.
pub(crate) fn run_auth(source: &Source) -> RunAuth<'_> {
    match &source.protocol {
        // The one mode whose credential is minted rather than stored, so the
        // placeholder in `api_key` says nothing about whether it has one.
        Protocol::AnthropicFederated(federation) => RunAuth {
            mode: MODE_FEDERATED,
            organization_id: Some(&federation.organization_id),
            service_account_id: Some(&federation.service_account_id),
            // Left out of the grant when the rule covers one workspace, and left
            // out of the report for the same reason.
            workspace_id: federation.workspace_id.as_deref(),
            federation_rule_id: Some(&federation.rule_id),
        },
        _ if is_placeholder(&source.api_key) => RunAuth::mode_only(MODE_NONE),
        Protocol::AnthropicOAuth => RunAuth::mode_only(MODE_OAUTH),
        Protocol::AnthropicApiKey | Protocol::OpenAiCompat => RunAuth::mode_only(MODE_API_KEY),
    }
}

/// Build the auth headers for an Anthropic request.
///
/// `bearer` is ignored in `AnthropicApiKey` mode and required in the bearer
/// modes. Returns an error rather than sending a placeholder credential.
pub(super) fn auth_headers(
    protocol: &Protocol,
    api_key: &str,
    bearer: Option<&str>,
) -> Result<HeaderMap, ClientError> {
    let mut pairs: Vec<(&str, String)> = vec![("anthropic-version", ANTHROPIC_VERSION.to_string())];
    match protocol {
        Protocol::AnthropicApiKey => {
            pairs.push(("x-api-key", usable(api_key, "API key")?));
        }
        Protocol::AnthropicOAuth | Protocol::AnthropicFederated(_) => {
            let token = usable(bearer.unwrap_or_default(), "bearer token")?;
            pairs.push(("authorization", format!("Bearer {token}")));
            pairs.push(("anthropic-beta", OAUTH_BETA.to_string()));
            // No `x-api-key`: see the module docs.
        }
        Protocol::OpenAiCompat => {
            return Err(ClientError::Internal(
                "auth_headers called for a non-Anthropic source".to_string(),
            ));
        }
    }
    into_header_map(pairs)
}

/// Whether a stored credential is really no credential at all.
///
/// One definition, shared by the wire path and the run summary, so a source afi
/// refuses to authenticate cannot be reported as having a key.
fn is_placeholder(credential: &str) -> bool {
    credential.is_empty() || credential == NOOP_KEY
}

/// Reject the placeholder and blanks before they reach the wire.
fn usable(credential: &str, label: &str) -> Result<String, ClientError> {
    if is_placeholder(credential) {
        return Err(ClientError::Auth(format!(
            "no Anthropic {label} configured. Set ANTHROPIC_API_KEY, \
             ANTHROPIC_AUTH_TOKEN, or the ANTHROPIC_FEDERATION_* variables."
        )));
    }
    Ok(credential.to_string())
}

fn into_header_map(pairs: Vec<(&str, String)>) -> Result<HeaderMap, ClientError> {
    let mut map = HeaderMap::new();
    for (name, value) in pairs {
        // Neither failure is a wire-parse problem, so neither is reported as one.
        // The names are this module's own literals, so a rejected one is a bug;
        // the values are credentials, and one copied with a trailing newline
        // lands here.
        let header = HeaderName::try_from(name)
            .map_err(|e| ClientError::Internal(format!("bad header name {name}: {e}")))?;
        // The error deliberately omits the value: it may be a credential.
        let header_value = HeaderValue::try_from(value)
            .map_err(|_| ClientError::Auth(format!("invalid characters in {name} value")))?;
        map.insert(header, header_value);
    }
    Ok(map)
}

#[derive(Debug, Clone)]
struct CachedToken {
    value: String,
    expires_at: Instant,
}

/// Per-source cache of minted federation tokens.
///
/// Federated tokens are short-lived, which is fine for a one-shot CI run but
/// would 401 partway through a long interactive session, so they are re-minted
/// as they approach expiry.
#[derive(Debug, Default)]
pub(crate) struct TokenCache {
    entries: RwLock<HashMap<String, CachedToken>>,
}

impl TokenCache {
    /// The bearer token to use for `source`, minting one if needed.
    pub(super) async fn bearer(
        &self,
        http: &Client,
        source: &Source,
    ) -> Result<String, ClientError> {
        match &source.protocol {
            Protocol::AnthropicOAuth => Ok(source.api_key.clone()),
            Protocol::AnthropicFederated(federation) => {
                self.federated(http, source, federation).await
            }
            _ => Err(ClientError::Internal(
                "bearer token requested for a source that does not use one".to_string(),
            )),
        }
    }

    async fn federated(
        &self,
        http: &Client,
        source: &Source,
        federation: &Federation,
    ) -> Result<String, ClientError> {
        if let Some(token) = self.fresh(&source.name).await {
            return Ok(token);
        }
        // Resolved at source discovery from the merged env map, so a value set in
        // `~/.env` or `AFI_ENV_FILE` is honoured - reading the process env here
        // would silently miss it.
        let identity = federation.identity.as_ref().ok_or_else(|| {
            ClientError::Auth(
                "no OIDC identity token available for the federated Anthropic source. \
                 Set ANTHROPIC_IDENTITY_TOKEN or ANTHROPIC_IDENTITY_TOKEN_FILE, or run \
                 inside GitHub Actions with `permissions: id-token: write`."
                    .to_string(),
            )
        })?;
        let assertion = fetch_identity_token(http, identity).await?;
        let minted = exchange(http, &source.base_url, federation, &assertion).await?;
        self.store(&source.name, &minted).await;
        Ok(minted.value)
    }

    async fn fresh(&self, name: &str) -> Option<String> {
        let entries = self.entries.read().await;
        let cached = entries.get(name)?;
        (cached.expires_at > Instant::now()).then(|| cached.value.clone())
    }

    async fn store(&self, name: &str, token: &CachedToken) {
        self.entries
            .write()
            .await
            .insert(name.to_string(), token.clone());
    }
}

/// Fetch the OIDC identity token, rejecting a blank one.
///
/// A blank token is a local misconfiguration - an empty or whitespace-only token
/// file, or an Actions response carrying an empty `value`. Left alone it would
/// reach the exchange as an empty `assertion`, and the 400 that comes back names
/// the grant rather than the empty file, hiding the real cause.
async fn fetch_identity_token(
    http: &Client,
    source: &IdentitySource,
) -> Result<String, ClientError> {
    let token = read_identity_token(http, source).await?;
    let token = token.trim();
    if token.is_empty() {
        return Err(ClientError::Auth(format!(
            "the OIDC identity token from {} is empty",
            identity_label(source)
        )));
    }
    Ok(token.to_string())
}

/// Names the configured identity source for an error message. Never includes the
/// token itself: it is a bearer credential in its own right.
fn identity_label(source: &IdentitySource) -> String {
    match source {
        IdentitySource::Literal(_) => "ANTHROPIC_IDENTITY_TOKEN".to_string(),
        IdentitySource::File(path) => {
            format!("ANTHROPIC_IDENTITY_TOKEN_FILE {}", path.display())
        }
        IdentitySource::GithubActions { .. } => "the GitHub Actions OIDC endpoint".to_string(),
    }
}

/// Read the raw token. Never logged.
async fn read_identity_token(
    http: &Client,
    source: &IdentitySource,
) -> Result<String, ClientError> {
    match source {
        IdentitySource::Literal(token) => Ok(token.clone()),
        IdentitySource::File(path) => fs::read_to_string(path).map_err(|e| {
            ClientError::Auth(format!(
                "cannot read ANTHROPIC_IDENTITY_TOKEN_FILE {}: {e}",
                path.display()
            ))
        }),
        IdentitySource::GithubActions { url, request_token } => {
            github_identity_token(http, url, request_token).await
        }
    }
}

async fn github_identity_token(
    http: &Client,
    url: &str,
    request_token: &str,
) -> Result<String, ClientError> {
    // The Actions runtime url already carries an `api-version` query, so the
    // audience is appended rather than replacing the query string.
    let mut endpoint = Url::parse(url)
        // Supplied by the Actions runtime, so a malformed one is environmental.
        .map_err(|e| ClientError::Auth(format!("bad ACTIONS_ID_TOKEN_REQUEST_URL: {e}")))?;
    endpoint
        .query_pairs_mut()
        .append_pair("audience", OIDC_AUDIENCE);
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
        // The body here is an Actions error, not a credential. A 403 is the
        // workflow lacking `permissions: id-token: write`.
        return Err(refused_credential(
            "the GitHub Actions OIDC endpoint refused the token request",
            status,
            &body,
        ));
    }
    json_string_field(&body, "value")
        .ok_or_else(|| ClientError::Parse("OIDC response had no `value` field".to_string()))
}

/// Exchange the identity token for an Anthropic access token.
///
/// This is the same JWT-bearer grant the official SDKs perform internally.
async fn exchange(
    http: &Client,
    base_url: &str,
    federation: &Federation,
    assertion: &str,
) -> Result<CachedToken, ClientError> {
    let mut body = serde_json::json!({
        "grant_type": "urn:ietf:params:oauth:grant-type:jwt-bearer",
        "assertion": assertion,
        "federation_rule_id": federation.rule_id,
        "organization_id": federation.organization_id,
        "service_account_id": federation.service_account_id,
    });
    if let Some(workspace) = &federation.workspace_id {
        body["workspace_id"] = Value::from(workspace.clone());
    }
    let response = http
        .post(super::token_url(base_url))
        .header("anthropic-beta", FEDERATION_BETA)
        .timeout(Duration::from_secs(30))
        .json(&body)
        .send()
        .await
        .map_err(|e| transport_error_at(Some("token exchange failed"), &e))?;
    let status = response.status();
    let text = response.text().await.map_err(|e| transport_error(&e))?;
    if !status.is_success() {
        return Err(refused_credential(
            "the Anthropic token exchange refused the identity token",
            status,
            &text,
        ));
    }
    parse_minted(&text)
}

/// A failing status from an endpoint that hands out credentials.
///
/// A 4xx here is the credential itself being turned down - a 401 from the exchange
/// almost always means the OIDC claims did not satisfy the federation rule, most
/// often an unprotected ref - so it reports as an auth failure rather than as the
/// transport error it arrives as. Retrying one spends the schedule to be refused
/// in the same words. A 429 or a 5xx is the endpoint having a bad day and keeps
/// its status, because that one is worth another attempt.
fn refused_credential(what: &str, status: StatusCode, body: &str) -> ClientError {
    if status.is_client_error() && status != StatusCode::TOO_MANY_REQUESTS {
        let preview: String = body.chars().take(BODY_PREVIEW_CHARS).collect();
        return ClientError::Auth(format!("{what} (HTTP {}): {preview}", status.as_u16()));
    }
    ClientError::Http {
        status: status.as_u16(),
        body: body.to_string(),
    }
}

fn parse_minted(body: &str) -> Result<CachedToken, ClientError> {
    let value = json_string_field(body, "access_token")
        .ok_or_else(|| ClientError::Parse("token exchange returned no access_token".to_string()))?;
    let lifetime = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get("expires_in").and_then(Value::as_u64))
        .unwrap_or(0);
    Ok(CachedToken {
        value,
        expires_at: Instant::now()
            .checked_add(Duration::from_secs(lifetime).saturating_sub(EXPIRY_SKEW))
            .unwrap_or_else(Instant::now),
    })
}

fn json_string_field(body: &str, key: &str) -> Option<String> {
    serde_json::from_str::<Value>(body)
        .ok()?
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

#[cfg(test)]
mod tests;
