//! Model sources: the `Source` endpoint type, HTTP-header/extra-body
//! helpers, and the context-window probe regex. `Runtime` and source
//! discovery live in the `runtime` submodule.

use std::collections::HashMap;
#[cfg(test)]
use std::hash::BuildHasher;

use regex::Regex;
use serde_json::{Map, Value};

mod args;
mod bedrock;
mod budget;
mod builtins;
mod effort;
mod file;
mod instructions;
mod protocol;
mod refusals;
mod runtime;
mod sources;
mod system_prompt;
mod tools;
mod window;
pub use args::{ParsedArgs, parse_args};
pub(crate) use bedrock::dns_suffix;
pub use bedrock::{Bedrock, WebIdentity};
pub use budget::Budget;
pub use effort::Effort;
pub use file::{FileSettings, Origin, config_files};
pub use instructions::nested;
pub use protocol::{
    ANTHROPIC_IDENTITY, AWS_IDENTITY, Federation, Identity, IdentitySource, IdentityVars, NOOP_KEY,
    Protocol, is_placeholder,
};
pub use runtime::Runtime;
pub use sources::discover_sources;
pub(crate) use sources::source_prefix;
pub use system_prompt::SystemPrompt;

use crate::summary::RunAuth;

/// Resolve a budget the way a run does, for tests that live outside this module.
///
/// `budget::resolve` is `pub(super)` because nothing but `Runtime` has any
/// business calling it, and `cost`'s tests need a real `Budget` rather than a
/// second constructor that could disagree with the one a run uses.
#[cfg(test)]
pub(crate) fn resolve_budget_for_tests<S: BuildHasher>(
    usd: &str,
    env: &HashMap<String, String, S>,
) -> Budget {
    budget::resolve(Some(usd), env)
        .expect("the fixture must resolve")
        .expect("the fixture must set a budget")
}

// --- HTTP headers for aggregators like OpenRouter ---------------------------

/// Build default HTTP headers (`HTTP-Referer`, `X-Title`) for API requests.
/// Returns `None` when neither is provided - the `OpenAI` client is then
/// constructed without `default_headers`.
#[must_use]
pub fn build_http_headers(
    app_name: Option<&str>,
    app_url: Option<&str>,
) -> Option<HashMap<String, String>> {
    let mut h = HashMap::new();
    if let Some(url) = app_url {
        h.insert("HTTP-Referer".to_string(), url.to_string());
    }
    if let Some(name) = app_name {
        h.insert("X-Title".to_string(), name.to_string());
    }
    if h.is_empty() { None } else { Some(h) }
}

// --- EXTRA_BODY parsing ------------------------------------------------------

/// Parse a `AFI_SOURCE_<NAME>_EXTRA_BODY` env value into a JSON object, or
/// `None`. Bad JSON is warned to stderr and ignored so a typo never silently
/// drops routing - it fails loudly at load.
#[must_use]
pub fn parse_extra_body(raw: Option<&str>) -> Option<Value> {
    let raw = match raw {
        Some(r) if !r.trim().is_empty() => r,
        _ => return None,
    };
    match serde_json::from_str::<Value>(raw) {
        Ok(Value::Object(m)) if !m.is_empty() => Some(Value::Object(m)),
        Ok(Value::Object(_)) => None, // empty {} -> no routing, same as unset
        Ok(other) => {
            eprintln!(
                "afi: AFI_*_EXTRA_BODY must be a JSON object, ignoring (got {}); \
                 provider routing is NOT set",
                type_name(&other)
            );
            None
        }
        Err(e) => {
            eprintln!(
                "afi: ignoring bad AFI_*_EXTRA_BODY JSON ({} at char {}); \
                 provider routing is NOT set",
                e,
                e.line() // close enough to the Python `pos`; serde doesn't expose char offset
            );
            None
        }
    }
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

// --- Source ------------------------------------------------------------------

/// A configured model endpoint. Owns its `base_url`, `api_key`, and optional
/// model name. `context_window` is lazily probed and cached in phase 5.
#[derive(Debug, Clone)]
pub struct Source {
    pub name: String,
    pub base_url: String,
    pub api_key: String,
    pub model: Option<String>,
    pub http_headers: Option<HashMap<String, String>>,
    pub extra_body: Option<Value>,
    pub context_window: Option<u64>,
    pub protocol: Protocol,
}

impl Source {
    #[must_use]
    pub fn new(
        name: &str,
        base_url: String,
        api_key: Option<String>,
        model: Option<String>,
        extra_body: Option<Value>,
        http_headers: Option<HashMap<String, String>>,
    ) -> Self {
        let api_key = match api_key {
            Some(s) if !s.is_empty() => s,
            _ => NOOP_KEY.to_string(),
        };
        Source {
            name: name.to_string(),
            base_url,
            api_key,
            model,
            http_headers,
            extra_body,
            context_window: None,
            protocol: Protocol::default(),
        }
    }

    /// Set the wire protocol. Kept separate from `Source::new` so the
    /// six-argument constructor and its call sites stay untouched.
    #[must_use]
    pub fn with_protocol(mut self, protocol: Protocol) -> Self {
        self.protocol = protocol;
        self
    }

    /// True when this source speaks Anthropic's Messages API rather than
    /// `OpenAI`-compatible chat completions.
    #[must_use]
    pub fn is_anthropic(&self) -> bool {
        self.protocol.is_anthropic()
    }

    /// Describe the credential this source authenticates with, for the run
    /// summary.
    ///
    /// Sits beside [`Protocol`] and [`Federation`] because those are what it
    /// reads. The federated arm reports exactly the non-secret fields the token
    /// exchange puts in its grant, and both draw them from the same struct, so a
    /// field that leaves the grant cannot be left behind in the report. The
    /// assertion and the minted token are not among them and never may be - see
    /// [`RunAuth`].
    ///
    /// The ids are what the exchange *sends*, not what the response was scoped
    /// to. Anthropic returns a token and nothing to attribute it with, so a rule
    /// that resolves a single workspace server-side reports no `workspace_id`.
    #[must_use]
    pub fn run_auth(&self) -> RunAuth<'_> {
        match &self.protocol {
            // The one mode whose credential is minted rather than stored, so the
            // placeholder in `api_key` says nothing about whether it has one.
            Protocol::AnthropicFederated(federation) => RunAuth::Federated {
                organization_id: &federation.organization_id,
                service_account_id: &federation.service_account_id,
                // Left out of the grant when the rule covers one workspace, and
                // left out of the report for the same reason.
                workspace_id: federation.workspace_id.as_deref(),
                federation_rule_id: &federation.rule_id,
            },
            // Ahead of the placeholder guard below: this protocol has no static
            // key to store, so `api_key` holds the placeholder even though the
            // run is fully credentialed and billed to an AWS account.
            Protocol::Bedrock(bedrock) => bedrock.run_auth(),
            // The llama.cpp case: `Source::new` stores the placeholder when
            // nothing was configured, so a keyless local server would otherwise
            // claim a credential it never had.
            _ if is_placeholder(&self.api_key) => RunAuth::NoCredential,
            Protocol::AnthropicOAuth => RunAuth::OAuth,
            // A static key is a static key. `OpenAiCompat` differs from
            // `AnthropicApiKey` only in which header carries it.
            Protocol::AnthropicApiKey | Protocol::OpenAiCompat => RunAuth::ApiKey,
        }
    }

    /// Why this source cannot be used as configured, naming what is absent.
    /// `None` when it can.
    #[must_use]
    pub fn config_error(&self) -> Option<String> {
        self.protocol.config_error(&self.name)
    }

    /// True if this source points at a host on the local machine or LAN
    /// (llama.cpp, Ollama, etc.) rather than a remote API. Local servers
    /// expose `/v1/models` and `/props` cheaply; remote hosts return empty
    /// lists or 404 there, so the context-window probe order flips for them.
    #[must_use]
    pub fn is_local(&self) -> bool {
        let host = self.host();
        if host.is_empty() {
            return true;
        }
        if matches!(host.as_str(), "localhost" | "0.0.0.0" | "::1") {
            return true;
        }
        LAN_RE.with(|re| re.is_match(&host))
    }

    /// The host of this source's base url, lowercased and without port or path.
    #[must_use]
    pub fn host(&self) -> String {
        let lower = self.base_url.to_lowercase();
        let after_scheme = lower.split_once("://").map_or(lower.as_str(), |(_, h)| h);
        after_scheme
            .split('/')
            .next()
            .unwrap_or_default()
            .split(':')
            .next()
            .unwrap_or_default()
            .to_string()
    }

    /// True when this source points at `OpenAI`'s own API rather than one of
    /// the many servers that merely speak its protocol.
    ///
    /// The two have diverged: `OpenAI`'s reasoning models reject `max_tokens`
    /// and take `max_completion_tokens`, which no self-hosted server implements.
    /// Host rather than model, because the newer spelling is right for every
    /// model on that host, and a list of model names would go stale.
    #[must_use]
    pub fn is_openai(&self) -> bool {
        self.host() == "api.openai.com"
    }

    /// Shorten a server-advertised model id into a concise display name. Only
    /// touches local file paths (`.gguf`, `.bin`, `.safetensors`); org/model
    /// ids (`zai-org/GLM-5.2`) are returned unchanged.
    #[must_use]
    pub fn clean_model_id(advertised: &str) -> String {
        if advertised.is_empty() {
            return advertised.to_string();
        }
        let lower = advertised.to_lowercase();
        if ![".gguf", ".bin", ".safetensors"]
            .iter()
            .any(|ext| lower.ends_with(ext))
        {
            return advertised.to_string();
        }
        let name = advertised.rsplit('/').next().unwrap_or(advertised);
        // Strip shard suffix on multi-file quantizations (e.g. "-00001-of-00009").
        // Rust's regex crate has no lookahead, so we capture the trailing dot or
        // end-of-string and re-insert it (matches the Python `(?=$|\.)`).
        let name = SHARD_RE.with(|re| re.replace_all(name, "$1")).to_string();
        let lower = name.to_lowercase();
        for ext in [".gguf", ".bin", ".safetensors"] {
            if lower.ends_with(ext) {
                return name[..name.len() - ext.len()].to_string();
            }
        }
        name
    }

    /// Resolve the model id this source will use. Phase 1 stub: returns the
    /// configured model, or `"local-model"` when unset. Phase 5 adds the
    /// `/v1/models` + `/props` network probes.
    #[must_use]
    pub fn resolve_model(&self) -> String {
        self.model
            .clone()
            .unwrap_or_else(|| "local-model".to_string())
    }

    /// Display name for the footer / `/source` list: the configured model or
    /// `"auto"` when unset.
    #[must_use]
    pub fn display_model(&self) -> String {
        self.model.clone().unwrap_or_else(|| "auto".to_string())
    }

    /// Current `provider.order` list, or `[]` when unset. Read-only accessor
    /// for the `/provider` listing.
    #[must_use]
    pub fn provider_order(&self) -> Vec<String> {
        let Some(Value::Object(body)) = &self.extra_body else {
            return vec![];
        };
        match body.get("provider") {
            Some(Value::Object(p)) => match p.get("order") {
                Some(Value::Array(a)) => a
                    .iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect(),
                _ => vec![],
            },
            _ => vec![],
        }
    }

    /// Set the `provider.order` routing for this source in place.
    ///
    /// A non-empty `order` pins routing to exactly those providers and,
    /// unless `allow_fallbacks` is explicitly true, disables `OpenRouter`'s
    /// usual fallback. An empty `order` clears routing entirely
    /// (`extra_body = None`).
    pub fn set_provider_order(&mut self, order: &[String], allow_fallbacks: Option<bool>) {
        if order.is_empty() {
            self.extra_body = None;
            return;
        }
        let mut body = match &self.extra_body {
            Some(Value::Object(m)) => m.clone(),
            _ => Map::new(),
        };
        let mut prov = match body.get("provider") {
            Some(Value::Object(p)) => p.clone(),
            _ => Map::new(),
        };
        prov.insert(
            "order".to_string(),
            Value::Array(order.iter().map(|s| Value::String(s.clone())).collect()),
        );
        let allow = allow_fallbacks.unwrap_or(false);
        prov.insert("allow_fallbacks".to_string(), Value::Bool(allow));
        body.insert("provider".to_string(), Value::Object(prov));
        self.extra_body = Some(Value::Object(body));
    }
}

thread_local! {
    static LAN_RE: Regex = Regex::new(
        r"^(127\.|10\.|192\.168\.|169\.254\.|172\.(1[6-9]|2[0-9]|3[01])\.)"
    ).unwrap();
    static SHARD_RE: Regex = Regex::new(r"-\d{5}-of-\d{5}(\.|$)").unwrap();
    static CTX_LIMIT_RE: Regex =
        Regex::new(r"(?i)maximum context length is (\d+) tokens").unwrap();
}

/// Context-window overrun-probe regex (used by phase 5). Exposed as a clone
/// for now since `thread_local!` refs can't escape the closure.
#[must_use]
pub fn ctx_limit_regex() -> Regex {
    CTX_LIMIT_RE.with(Clone::clone)
}

#[cfg(test)]
mod tests;
