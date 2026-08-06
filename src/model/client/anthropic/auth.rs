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
use reqwest::{Client, Url};
use serde_json::Value;
use tokio::sync::RwLock;

use crate::config::{Federation, IdentitySource, NOOP_KEY, Protocol, Source};
use crate::model::client::ClientError;

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
            return Err(ClientError::Config(
                "auth_headers called for a non-Anthropic source".to_string(),
            ));
        }
    }
    into_header_map(pairs)
}

/// Reject the placeholder and blanks before they reach the wire.
fn usable(credential: &str, label: &str) -> Result<String, ClientError> {
    if credential.is_empty() || credential == NOOP_KEY {
        return Err(ClientError::Config(format!(
            "no Anthropic {label} configured. Set ANTHROPIC_API_KEY, \
             ANTHROPIC_AUTH_TOKEN, or the ANTHROPIC_FEDERATION_* variables."
        )));
    }
    Ok(credential.to_string())
}

fn into_header_map(pairs: Vec<(&str, String)>) -> Result<HeaderMap, ClientError> {
    let mut map = HeaderMap::new();
    for (name, value) in pairs {
        let header = HeaderName::try_from(name)
            .map_err(|e| ClientError::Parse(format!("bad header name {name}: {e}")))?;
        // The error deliberately omits the value: it may be a credential.
        let header_value = HeaderValue::try_from(value)
            .map_err(|_| ClientError::Parse(format!("invalid characters in {name} value")))?;
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
            _ => Err(ClientError::Parse(
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
            ClientError::Config(
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

/// Fetch the raw OIDC identity token. Never logged: it is a bearer credential
/// in its own right.
async fn fetch_identity_token(
    http: &Client,
    source: &IdentitySource,
) -> Result<String, ClientError> {
    match source {
        IdentitySource::Literal(token) => Ok(token.clone()),
        IdentitySource::File(path) => fs::read_to_string(path)
            .map(|s| s.trim().to_string())
            .map_err(|e| {
                ClientError::Config(format!(
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
        .map_err(|e| ClientError::Parse(format!("bad ACTIONS_ID_TOKEN_REQUEST_URL: {e}")))?;
    endpoint
        .query_pairs_mut()
        .append_pair("audience", OIDC_AUDIENCE);
    let response = http
        .get(endpoint)
        .header("authorization", format!("Bearer {request_token}"))
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| ClientError::Connection(format!("OIDC token request failed: {e}")))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| ClientError::Connection(e.to_string()))?;
    if !status.is_success() {
        // The body here is an Actions error, not a credential.
        return Err(ClientError::Http {
            status: status.as_u16(),
            body,
        });
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
        .map_err(|e| ClientError::Connection(format!("token exchange failed: {e}")))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|e| ClientError::Connection(e.to_string()))?;
    if !status.is_success() {
        // A 401 here almost always means the OIDC claims did not satisfy the
        // federation rule - most often an unprotected ref.
        return Err(ClientError::Http {
            status: status.as_u16(),
            body: text,
        });
    }
    parse_minted(&text)
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
