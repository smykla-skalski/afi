//! How to reach a source: its wire protocol, the credential shape that
//! protocol implies, and the workload-identity-federation parameters.

use std::collections::HashMap;
use std::hash::BuildHasher;
use std::path::PathBuf;

/// Placeholder stored when a source has no API key. It must never reach the
/// wire as an Anthropic `x-api-key`: the Messages API validates that header in
/// preference to a bearer token, so a non-empty wrong value fails the request
/// outright even when a valid bearer is present.
pub const NOOP_KEY: &str = "sk-noop";

/// Whether a stored credential is really no credential at all.
///
/// `Source::new` substitutes [`NOOP_KEY`] when nothing was configured, so every
/// reader has to know that a key can be present and still be nothing. One
/// definition, beside the constant it tests for, keeps the wire path and the run
/// summary from drifting on what counts as unconfigured - a source afi refuses
/// to authenticate must not be reported as having a key.
#[must_use]
pub fn is_placeholder(credential: &str) -> bool {
    credential.is_empty() || credential == NOOP_KEY
}

/// Where the OIDC identity token for a federation exchange comes from.
///
/// Resolved here, from the same merged env map as every other setting, rather
/// than from the process env at request time - otherwise a value set in `~/.env`
/// or `AFI_ENV_FILE` would be invisible, since nothing copies those into the
/// process environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentitySource {
    /// `ANTHROPIC_IDENTITY_TOKEN` - the token itself.
    Literal(String),
    /// `ANTHROPIC_IDENTITY_TOKEN_FILE` - a path to read it from.
    File(PathBuf),
    /// GitHub Actions' OIDC endpoint, which is what `core.getIDToken` calls.
    GithubActions { url: String, request_token: String },
}

impl IdentitySource {
    /// Resolve an identity-token source, in the same precedence order the
    /// official SDKs use, with GitHub Actions as an afi-specific fallback so a
    /// workflow needs no explicit token-minting step.
    #[must_use]
    pub fn from_env<S: BuildHasher>(env: &HashMap<String, String, S>) -> Option<Self> {
        let get = |key: &str| env.get(key).map(String::as_str).filter(|v| !v.is_empty());
        if let Some(token) = get("ANTHROPIC_IDENTITY_TOKEN") {
            return Some(Self::Literal(token.to_string()));
        }
        if let Some(path) = get("ANTHROPIC_IDENTITY_TOKEN_FILE") {
            return Some(Self::File(PathBuf::from(path)));
        }
        let url = get("ACTIONS_ID_TOKEN_REQUEST_URL")?;
        let request_token = get("ACTIONS_ID_TOKEN_REQUEST_TOKEN")?;
        Some(Self::GithubActions {
            url: url.to_string(),
            request_token: request_token.to_string(),
        })
    }
}

/// Workload-identity-federation parameters for minting a short-lived Anthropic
/// access token from an OIDC identity token. The four ids are non-secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Federation {
    pub rule_id: String,
    pub organization_id: String,
    pub service_account_id: String,
    /// Only required when the federation rule spans multiple workspaces.
    pub workspace_id: Option<String>,
    /// Where to get the identity token to exchange. `None` when none is
    /// configured, which surfaces as an actionable error on the first request
    /// rather than hiding the source entirely.
    pub identity: Option<IdentitySource>,
}

impl Federation {
    /// Read the federation ids from an env map, using the same variable names as
    /// the official SDKs so a workspace already configured for them needs no
    /// afi-specific plumbing. Returns `None` unless all three required ids are
    /// present and non-empty.
    #[must_use]
    pub fn from_env<S: BuildHasher>(env: &HashMap<String, String, S>) -> Option<Self> {
        let get = |key: &str| {
            env.get(key)
                .map(String::as_str)
                .filter(|v| !v.is_empty())
                .map(String::from)
        };
        Some(Self {
            rule_id: get("ANTHROPIC_FEDERATION_RULE_ID")?,
            organization_id: get("ANTHROPIC_ORGANIZATION_ID")?,
            service_account_id: get("ANTHROPIC_SERVICE_ACCOUNT_ID")?,
            workspace_id: get("ANTHROPIC_WORKSPACE_ID"),
            identity: IdentitySource::from_env(env),
        })
    }
}

/// How to reach a source: its wire protocol together with the credential shape
/// that protocol implies.
///
/// Protocol and auth are one enum rather than two fields because they are not
/// independent - there is no such thing as an `OpenAI`-compatible endpoint
/// authenticated with an Anthropic OAuth bearer. Folding them removes the
/// nonsense states entirely.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Protocol {
    /// `OpenAI`-compatible `/chat/completions`, `Authorization: Bearer <key>`.
    #[default]
    OpenAiCompat,
    /// Anthropic Messages API, `x-api-key` plus `anthropic-version`.
    AnthropicApiKey,
    /// Anthropic Messages API with a pre-minted OAuth bearer token.
    /// `x-api-key` is never sent in this mode.
    AnthropicOAuth,
    /// Anthropic Messages API, minting a bearer token via workload identity
    /// federation before each expiry. `x-api-key` is never sent.
    AnthropicFederated(Box<Federation>),
}

impl Protocol {
    /// True when this source speaks Anthropic's Messages API rather than
    /// `OpenAI`-compatible chat completions.
    #[must_use]
    pub fn is_anthropic(&self) -> bool {
        !matches!(self, Self::OpenAiCompat)
    }

    /// True when auth rides on `Authorization: Bearer` and `x-api-key` must be
    /// omitted entirely.
    #[must_use]
    pub fn is_bearer(&self) -> bool {
        matches!(self, Self::AnthropicOAuth | Self::AnthropicFederated(_))
    }

    /// Parse an `AFI_SOURCE_<NAME>_PROTOCOL` value. Unknown values warn to
    /// stderr and fall back to `OpenAiCompat` so a typo never silently
    /// reroutes a source. Federated auth is not reachable from this knob - it
    /// needs the federation ids, which only the built-in source resolves.
    #[must_use]
    pub fn from_env_value(raw: &str) -> Self {
        match raw.trim().to_lowercase().as_str() {
            "" | "openai" | "openai-compat" => Self::OpenAiCompat,
            "anthropic" | "anthropic-api-key" => Self::AnthropicApiKey,
            "anthropic-oauth" => Self::AnthropicOAuth,
            other => {
                eprintln!(
                    "afi: unknown AFI_SOURCE_*_PROTOCOL {other:?}, using openai; \
                     expected one of openai, anthropic, anthropic-oauth"
                );
                Self::OpenAiCompat
            }
        }
    }
}
