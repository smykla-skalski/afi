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

use std::time::{Duration, Instant};

use reqwest::Client;
use reqwest::header::HeaderMap;
use serde_json::Value;

use crate::config::{ANTHROPIC_IDENTITY, Federation, Identity, Protocol, Source, is_placeholder};
use crate::model::client::expiry::{Expiring, deadline};
use crate::model::client::identity::{fetch, json_string_field, refused_credential};
use crate::model::client::{
    ClientError, Credential, Redactor, into_header_map, transport_error, transport_error_at,
};

/// Pinned API version. Anthropic requires this on every request.
const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Required on `/v1/messages` whenever auth is a bearer token rather than a key.
const OAUTH_BETA: &str = "oauth-2025-04-20";
/// Additionally required on the token-exchange endpoint.
const FEDERATION_BETA: &str = "oauth-2025-04-20,oidc-federation-2026-04-01";

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
        Protocol::OpenAiCompat | Protocol::Bedrock(_) => {
            return Err(ClientError::Internal(
                "auth_headers called for a non-Anthropic source".to_string(),
            ));
        }
    }
    into_header_map(pairs)
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

/// A minted token and the deadline it stops working at.
type Minted = (String, Instant);

/// Per-source cache of minted federation tokens.
///
/// Federated tokens are short-lived, which is fine for a one-shot CI run but
/// would 401 partway through a long interactive session, so they are re-minted
/// as they approach expiry. When that is lives in [`super::super::expiry`],
/// which the Bedrock role assumption answers the same question through.
#[derive(Debug, Default)]
pub(crate) struct TokenCache {
    tokens: Expiring<String>,
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
        if let Some(token) = self.tokens.fresh(&source.name).await {
            return Ok(token);
        }
        // Resolved at source discovery from the merged env map, so a value set in
        // `~/.env` or `AFI_ENV_FILE` is honoured - reading the process env here
        // would silently miss it.
        let identity = federation.identity.as_ref().ok_or_else(|| {
            // The sentence comes from the variables rather than being written
            // out here, so renaming one cannot leave this message naming the
            // old spelling while every other refusal moves.
            ClientError::Auth(format!(
                "source {} federates with Anthropic but {}",
                source.name,
                Identity::absent(ANTHROPIC_IDENTITY)
            ))
        })?;
        let assertion = fetch(http, identity).await?;
        let (token, expires_at) = exchange(http, &source.base_url, federation, &assertion).await?;
        self.tokens
            .store(&source.name, token.clone(), expires_at)
            .await;
        Ok(token)
    }
}

/// Exchange the identity token for an Anthropic access token.
///
/// This is the same JWT-bearer grant the official SDKs perform internally.
async fn exchange(
    http: &Client,
    base_url: &str,
    federation: &Federation,
    assertion: &str,
) -> Result<Minted, ClientError> {
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
        // The assertion was in the body that was just posted, so a rejected grant
        // echoed back carries it. This is the leak the redaction exists for.
        return Err(refused_credential(
            "the Anthropic token exchange refused the identity token",
            status,
            &text,
            &Redactor::default().with(assertion, Credential::IdentityToken),
        ));
    }
    parse_minted(&text)
}

/// The token and how long it lasts.
///
/// A response carrying no `expires_in` reads as a zero lifetime, which
/// [`deadline`] turns into a deadline already past: the token serves this
/// request and is re-minted for the next one.
fn parse_minted(body: &str) -> Result<Minted, ClientError> {
    let value = json_string_field(body, "access_token")
        .ok_or_else(|| ClientError::Parse("token exchange returned no access_token".to_string()))?;
    let lifetime = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get("expires_in").and_then(Value::as_u64))
        .unwrap_or(0);
    Ok((value, deadline(Duration::from_secs(lifetime))))
}

#[cfg(test)]
mod tests;
