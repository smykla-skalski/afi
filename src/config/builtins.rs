//! Built-in convenience sources.
//!
//! Each vendor here auto-registers when its API key is present, so a bare
//! `AFI_<VENDOR>_API_KEY` is enough to get a working source with sensible
//! defaults. They are appended *after* explicitly configured sources so a
//! hand-written `AFI_SOURCE_<NAME>_*` block always wins, and so they never
//! displace the startup default.

use std::collections::HashMap;
use std::hash::BuildHasher;

use crate::envfile;

use super::{Federation, Protocol, Source, build_http_headers, parse_extra_body};

/// Anthropic's API root. Deliberately without a `/v1` suffix; the client
/// normalizes either form.
const ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";
/// Current Sonnet generation: near-Opus quality on coding and agentic work at
/// Sonnet pricing. Override with `AFI_ANTHROPIC_MODEL`.
const ANTHROPIC_MODEL: &str = "claude-sonnet-5";

/// Register every built-in source whose credentials are configured.
pub(super) fn add_builtin_sources<S: BuildHasher>(
    env: &HashMap<String, String, S>,
    sources: &mut HashMap<String, Source>,
    order: &mut Vec<String>,
) {
    add_together_source(env, sources, order);
    add_openrouter_source(env, sources, order);
    add_anthropic_source(env, sources, order);
}

/// Register the built-in `anthropic` source when any credential resolves.
///
/// The un-prefixed `ANTHROPIC_*` fallbacks are a deliberate exception to the
/// `AFI_*`-only convention: a CI job or shell that already exports them for the
/// official SDKs or the `ant` CLI then works with no afi-specific setup.
fn add_anthropic_source<S: BuildHasher>(
    env: &HashMap<String, String, S>,
    sources: &mut HashMap<String, Source>,
    order: &mut Vec<String>,
) {
    if sources.contains_key("anthropic") {
        return;
    }
    let Some((protocol, credential)) = anthropic_auth(env) else {
        return;
    };
    // These tweak the built-in, so they deliberately avoid the `AFI_SOURCE_*`
    // namespace. A bare `AFI_SOURCE_ANTHROPIC_BASE_URL` is enough for
    // `source_names` to auto-discover a source called `anthropic`, which would
    // reach here already present and short-circuit the return above - leaving an
    // `OpenAiCompat` source holding the `sk-noop` placeholder. Setting a full
    // `AFI_SOURCE_ANTHROPIC_*` block is still supported, but then its
    // `_PROTOCOL` selects the wire protocol, as for any other named source.
    let base_url = env_value(env, &["AFI_ANTHROPIC_BASE_URL", "ANTHROPIC_BASE_URL"])
        .unwrap_or_else(|| ANTHROPIC_BASE_URL.to_string());
    let model =
        env_value(env, &["AFI_ANTHROPIC_MODEL"]).unwrap_or_else(|| ANTHROPIC_MODEL.to_string());
    let extra_body = parse_extra_body(env.get("AFI_ANTHROPIC_EXTRA_BODY").map(String::as_str));
    let src = Source::new(
        "anthropic",
        base_url,
        credential,
        Some(model),
        extra_body,
        None,
    )
    .with_protocol(protocol);
    sources.insert("anthropic".to_string(), src);
    order.push("anthropic".to_string());
}

/// Resolve which credential the `anthropic` source will use.
///
/// Precedence matches the official SDKs: a static API key, then a pre-minted
/// bearer token, then workload identity federation. Returns the protocol plus
/// the credential to store, or `None` when nothing is configured - in which case
/// the source is not registered at all.
fn anthropic_auth<S: BuildHasher>(
    env: &HashMap<String, String, S>,
) -> Option<(Protocol, Option<String>)> {
    if let Some(key) = resolved(env, &["AFI_ANTHROPIC_API_KEY", "ANTHROPIC_API_KEY"]) {
        return Some((Protocol::AnthropicApiKey, Some(key)));
    }
    if let Some(token) = resolved(env, &["AFI_ANTHROPIC_OAUTH_TOKEN", "ANTHROPIC_AUTH_TOKEN"]) {
        return Some((Protocol::AnthropicOAuth, Some(token)));
    }
    // No static credential: mint one per session from an OIDC identity token.
    let federation = Federation::from_env(env)?;
    Some((Protocol::AnthropicFederated(Box::new(federation)), None))
}

/// First non-empty value among `keys`, resolving `$NAME` indirection so a
/// credential can point at another variable like the other built-ins allow.
fn resolved<S: BuildHasher>(env: &HashMap<String, String, S>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        envfile::resolve_api_key(env, env.get(*key).map(String::as_str)).filter(|v| !v.is_empty())
    })
}

/// First non-empty plain value among `keys`.
fn env_value<S: BuildHasher>(env: &HashMap<String, String, S>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| env.get(*key).filter(|v| !v.is_empty()).cloned())
}

/// Register the built-in `together` source when `AFI_TOGETHER_API_KEY` is set.
fn add_together_source<S: BuildHasher>(
    env: &HashMap<String, String, S>,
    sources: &mut HashMap<String, Source>,
    order: &mut Vec<String>,
) {
    if sources.contains_key("together") {
        return;
    }
    let Some(key) =
        envfile::resolve_api_key(env, env.get("AFI_TOGETHER_API_KEY").map(String::as_str))
            .filter(|k| !k.is_empty())
    else {
        return;
    };
    let src = Source::new(
        "together",
        "https://api.together.xyz/v1".to_string(),
        Some(key),
        Some("zai-org/GLM-5.2".to_string()),
        None,
        None,
    );
    sources.insert("together".to_string(), src);
    order.push("together".to_string());
}

/// Register the built-in `openrouter` source when `AFI_OPENROUTER_API_KEY` is set.
fn add_openrouter_source<S: BuildHasher>(
    env: &HashMap<String, String, S>,
    sources: &mut HashMap<String, Source>,
    order: &mut Vec<String>,
) {
    if sources.contains_key("openrouter") {
        return;
    }
    let Some(key) =
        envfile::resolve_api_key(env, env.get("AFI_OPENROUTER_API_KEY").map(String::as_str))
            .filter(|k| !k.is_empty())
    else {
        return;
    };
    let or_body = parse_extra_body(
        env.get("AFI_SOURCE_OPENROUTER_EXTRA_BODY")
            .map(String::as_str),
    )
    .unwrap_or_else(|| {
        serde_json::json!({
            "provider": { "order": ["parasail/fp8"], "allow_fallbacks": false }
        })
    });
    let app_name = env
        .get("AFI_SOURCE_OPENROUTER_APP_NAME")
        .map(String::as_str)
        .or(Some("Afi"));
    let app_url = env
        .get("AFI_SOURCE_OPENROUTER_APP_URL")
        .map(String::as_str)
        .or(Some("https://github.com/smykla-skalski/afi"));
    let headers = build_http_headers(app_name, app_url);
    let src = Source::new(
        "openrouter",
        "https://openrouter.ai/api/v1".to_string(),
        Some(key),
        Some("z-ai/glm-5.2".to_string()),
        Some(or_body),
        headers,
    );
    sources.insert("openrouter".to_string(), src);
    order.push("openrouter".to_string());
}
