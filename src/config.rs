//! Model sources, runtime state, and source discovery.
//!
//! A "source" bundles a base_url, api_key, and optional model name. Define
//! sources with `AFI_SOURCES` + `AFI_SOURCE_<NAME>_*` env vars; the
//! built-in `together` and `openrouter` sources auto-register when their
//! keys are present. Switch at runtime with `switch_source` (the `/source`
//! slash command in the REPL).

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;

use crate::approval::{apply_approval, normalize_approval, ApprovalState};
use crate::envfile;

// --- HTTP headers for aggregators like OpenRouter ---------------------------

/// Build default HTTP headers (`HTTP-Referer`, `X-Title`) for API requests.
/// Returns `None` when neither is provided - the OpenAI client is then
/// constructed without `default_headers`.
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
    if h.is_empty() {
        None
    } else {
        Some(h)
    }
}

// --- EXTRA_BODY parsing ------------------------------------------------------

/// Parse a `AFI_SOURCE_<NAME>_EXTRA_BODY` env value into a JSON object, or
/// `None`. Bad JSON is warned to stderr and ignored so a typo never silently
/// drops routing - it fails loudly at load.
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
                "minion: AFI_*_EXTRA_BODY must be a JSON object, ignoring (got {}); \
                 provider routing is NOT set",
                type_name(&other)
            );
            None
        }
        Err(e) => {
            eprintln!(
                "minion: ignoring bad AFI_*_EXTRA_BODY JSON ({} at char {}); \
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

/// A configured model endpoint. Owns its base_url, api_key, and optional
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
}

impl Source {
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
            _ => "sk-noop".to_string(),
        };
        Source {
            name: name.to_string(),
            base_url,
            api_key,
            model,
            http_headers,
            extra_body,
            context_window: None,
        }
    }

    /// The `extra_body` kwarg to merge into every chat request, or `None` when
    /// none is configured. Non-consumers ignore unknown body keys, so this is
    /// a no-op for backends that don't understand it (llama.cpp, Together, ...).
    pub fn extra_request_kwargs(&self) -> Option<Value> {
        self.extra_body
            .as_ref()
            .map(|b| serde_json::json!({"extra_body": b.clone()}))
    }

    /// True if this source points at a host on the local machine or LAN
    /// (llama.cpp, Ollama, etc.) rather than a remote API. Local servers
    /// expose `/v1/models` and `/props` cheaply; remote hosts return empty
    /// lists or 404 there, so the context-window probe order flips for them.
    pub fn is_local(&self) -> bool {
        let host = self.base_url.to_lowercase();
        let host = match host.split_once("://") {
            Some((_, h)) => h,
            None => &host,
        };
        let host = host
            .split('/')
            .next()
            .unwrap_or("")
            .split(':')
            .next()
            .unwrap_or("");
        if host.is_empty() {
            return true;
        }
        if matches!(host, "localhost" | "0.0.0.0" | "::1") {
            return true;
        }
        LAN_RE.with(|re| re.is_match(host))
    }

    /// Shorten a server-advertised model id into a concise display name. Only
    /// touches local file paths (`.gguf`, `.bin`, `.safetensors`); org/model
    /// ids (`zai-org/GLM-5.2`) are returned unchanged.
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
    pub fn resolve_model(&self) -> String {
        self.model
            .clone()
            .unwrap_or_else(|| "local-model".to_string())
    }

    /// Display name for the footer / `/source` list: the configured model or
    /// `"auto"` when unset.
    pub fn display_model(&self) -> String {
        self.model.clone().unwrap_or_else(|| "auto".to_string())
    }

    /// Current `provider.order` list, or `[]` when unset. Read-only accessor
    /// for the `/provider` listing.
    pub fn provider_order(&self) -> Vec<String> {
        let body = match &self.extra_body {
            Some(Value::Object(m)) => m,
            _ => return vec![],
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
    /// unless `allow_fallbacks` is explicitly true, disables OpenRouter's
    /// usual fallback. An empty `order` clears routing entirely
    /// (`extra_body = None`).
    pub fn set_provider_order(&mut self, order: &[String], allow_fallbacks: Option<bool>) {
        if order.is_empty() {
            self.extra_body = None;
            return;
        }
        let mut body = match &self.extra_body {
            Some(Value::Object(m)) => m.clone(),
            _ => serde_json::Map::new(),
        };
        let mut prov = match body.get("provider") {
            Some(Value::Object(p)) => p.clone(),
            _ => serde_json::Map::new(),
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
#[allow(dead_code)]
pub fn ctx_limit_regex() -> Regex {
    CTX_LIMIT_RE.with(|re| re.clone())
}

// --- Runtime -----------------------------------------------------------------

/// The mutable state of a running session: sources, the active source, the
/// resolved model, approval mode, and CLI-derived flags. In the Python
/// original these were module globals (`SOURCES`, `ACTIVE`, `client`,
/// `MODEL`, `YOLO`, ...); here they live in one struct owned by `main` and
/// borrowed by the REPL.
#[derive(Debug, Clone)]
pub struct Runtime {
    pub sources: HashMap<String, Source>,
    pub source_order: Vec<String>,
    pub active: Option<String>,
    pub model: Option<String>,
    pub approval: ApprovalState,
    pub prompt_file: Option<String>,
    pub resume: Option<Option<String>>,
    pub session: Option<String>,
    pub env: HashMap<String, String>,
}

/// Parsed CLI args - the subset that affects initial state. The `sessions`
/// subcommand and in-REPL slash commands are handled separately.
#[derive(Debug, Default, Clone)]
pub struct ParsedArgs {
    pub source: Option<String>,
    pub yolo: bool,
    pub approval: Option<String>,
    pub resume: Option<Option<String>>,
    pub session: Option<String>,
    pub prompt_file: Option<String>,
    pub sessions_query: Option<Vec<String>>,
}

/// Parse argv into the subset that affects runtime construction.
///
/// Hand-rolled for now so tests can pass `["minion", "--source", "zai"]`
/// directly without a clap dependency at test time. Phase 8 may swap this for
/// a clap-based parser; the surface should stay byte-identical.
pub fn parse_args(args: &[String]) -> ParsedArgs {
    let mut out = ParsedArgs::default();
    let mut i = 1; // skip argv[0]
    let mut saw_sessions = false;
    let mut query: Vec<String> = Vec::new();
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--yolo" => out.yolo = true,
            "--approval" if i + 1 < args.len() => {
                out.approval = Some(args[i + 1].clone());
                i += 1;
            }
            "--source" if i + 1 < args.len() => {
                out.source = Some(args[i + 1].clone());
                i += 1;
            }
            "--session" if i + 1 < args.len() => {
                out.session = Some(args[i + 1].clone());
                i += 1;
            }
            "--prompt-file" | "-f" if i + 1 < args.len() => {
                out.prompt_file = Some(args[i + 1].clone());
                i += 1;
            }
            "--resume" | "-r" => {
                // bare --resume, or --resume <target> where target doesn't
                // start with '-' (so `--resume --yolo` doesn't swallow --yolo).
                if i + 1 < args.len() && !args[i + 1].starts_with('-') {
                    out.resume = Some(Some(args[i + 1].clone()));
                    i += 1;
                } else {
                    out.resume = Some(None);
                }
            }
            "sessions" if i == 1 => {
                saw_sessions = true;
            }
            other if saw_sessions => {
                query.push(other.to_string());
            }
            _ => {}
        }
        i += 1;
    }
    if saw_sessions {
        out.sessions_query = Some(query);
    }
    out
}

impl Runtime {
    /// Build a fresh runtime from argv, an env map, and an optional env file.
    ///
    /// `env` is the starting env (typically `std::env::vars()`); `env_file`
    /// is loaded and merged in without clobbering existing keys (matches the
    /// Python `~/.env` loader). Then sources are discovered, args parsed,
    /// approval defaults applied (env `AFI_APPROVAL` then `--approval` /
    /// `--yolo`), and the starting source selected (`--source` then
    /// `AFI_ACTIVE` then first in `AFI_SOURCES`).
    pub fn build(
        args: &[String],
        mut env: HashMap<String, String>,
        env_file: Option<&Path>,
    ) -> Self {
        if let Some(path) = env_file {
            envfile::load_into(&mut env, path);
        }

        let (sources, source_order) = discover_sources(&env);
        let parsed = parse_args(args);

        let mut approval = ApprovalState::default();
        if let Some(val) = env.get("AFI_APPROVAL") {
            if !val.trim().is_empty() {
                if let Some(kind) = normalize_approval(val) {
                    apply_approval(&mut approval, kind, true);
                } else {
                    eprintln!(
                        "  \u{2717} unknown AFI_APPROVAL={:?} \
                         (want all|low|medium|high|yolo); prompting for all actions",
                        val
                    );
                }
            }
        }
        if let Some(level) = &parsed.approval {
            if let Some(kind) = normalize_approval(level) {
                apply_approval(&mut approval, kind, true);
            } else {
                eprintln!(
                    "  \u{2717} unknown --approval level {:?} \
                     (want all|low|medium|high|yolo); keeping {}",
                    level,
                    crate::approval::approval_display(&approval)
                );
            }
        }
        if parsed.yolo {
            // bare --yolo: short-circuit everything, never prompt. Does NOT
            // touch default_approve_level (matches the Python --yolo path).
            approval.yolo = true;
            approval.approve_level = None;
        }

        let mut rt = Runtime {
            sources,
            source_order,
            active: None,
            model: None,
            approval,
            prompt_file: parsed.prompt_file,
            resume: parsed.resume,
            session: parsed.session,
            env,
        };

        let start = parsed
            .source
            .or_else(|| rt.env.get("AFI_ACTIVE").cloned())
            .or_else(|| rt.source_order.first().cloned());
        if let Some(name) = start {
            rt.switch_source(&name, None);
        }

        rt
    }

    /// Swap the active source. Reassigns `active` + `model`. Returns `false`
    /// (with a printed message) if the name is unknown.
    ///
    /// `model_override` (optional) pins `model` to a specific id for this
    /// switch - used by `/source <name> <model>` so a multi-model host can be
    /// pointed at any of its models without a config edit. A bare switch (no
    /// override) always falls back to the source's configured default.
    pub fn switch_source(&mut self, name: &str, model_override: Option<&str>) -> bool {
        if !self.sources.contains_key(name) {
            eprintln!("  \u{2717} unknown source {:?}", name);
            return false;
        }
        self.active = Some(name.to_string());
        let model = match model_override {
            Some(m) => m.to_string(),
            None => self.sources[name].resolve_model(),
        };
        self.model = Some(model);
        // A model change means the cached max-context may no longer apply - drop
        // it so the footer re-probes against the new model next turn.
        if let Some(s) = self.sources.get_mut(name) {
            s.context_window = None;
        }
        true
    }

    /// Best-effort restore of the source (and optional model) a session was
    /// started on, used when resuming. Returns `true` if the source is now the
    /// requested one.
    pub fn restore_source(&mut self, source_name: Option<&str>, model: Option<&str>) -> bool {
        let name = match source_name {
            Some(n) if self.sources.contains_key(n) => n.to_string(),
            _ => return false,
        };
        let src = &self.sources[&name];
        let pin = match model {
            Some(m) if Some(m) != src.model.as_deref() => Some(m.to_string()),
            _ => None,
        };
        if self.active.as_deref() == Some(&name)
            && self.model.as_deref() == pin.as_deref().or(src.model.as_deref())
        {
            return true;
        }
        self.switch_source(&name, pin.as_deref());
        true
    }

    /// Borrow the active source, if any.
    pub fn active_source(&self) -> Option<&Source> {
        self.active.as_ref().and_then(|n| self.sources.get(n))
    }
}

// --- Source discovery --------------------------------------------------------

/// Build the sources map + ordered list from `AFI_SOURCE_*` env vars,
/// falling back to a single `local` source from the legacy `AFI_*` vars.
pub fn discover_sources(env: &HashMap<String, String>) -> (HashMap<String, Source>, Vec<String>) {
    let mut sources: HashMap<String, Source> = HashMap::new();
    let mut source_order: Vec<String> = Vec::new();

    let names: Vec<String> = match env.get("AFI_SOURCES") {
        Some(raw) if !raw.trim().is_empty() => raw
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        _ => {
            // auto-discover from AFI_SOURCE_*_BASE_URL
            let prefix = "AFI_SOURCE_";
            let suffix = "_BASE_URL";
            let mut found: Vec<String> = env
                .keys()
                .filter_map(|k| {
                    k.strip_prefix(prefix)
                        .and_then(|rest| rest.strip_suffix(suffix))
                        .map(|name| name.to_lowercase())
                })
                .collect();
            found.sort();
            found
        }
    };

    for name in &names {
        let p = format!("AFI_SOURCE_{}_", name.to_uppercase());
        let base_url = match env.get(&(p.clone() + "BASE_URL")) {
            Some(s) => s.clone(),
            None => continue,
        };
        let api_key =
            envfile::resolve_api_key(env, env.get(&(p.clone() + "API_KEY")).map(|s| s.as_str()));
        let model = env.get(&(p.clone() + "MODEL")).cloned();
        let extra_body = parse_extra_body(env.get(&(p.clone() + "EXTRA_BODY")).map(|s| s.as_str()));
        let http_headers = build_http_headers(
            env.get(&(p.clone() + "APP_NAME")).map(|s| s.as_str()),
            env.get(&(p.clone() + "APP_URL")).map(|s| s.as_str()),
        );
        let src = Source::new(name, base_url, api_key, model, extra_body, http_headers);
        sources.insert(name.clone(), src);
        source_order.push(name.clone());
    }

    if sources.is_empty() {
        // legacy fallback: one source from AFI_BASE_URL etc.
        let src = Source::new(
            "local",
            env.get("AFI_BASE_URL")
                .cloned()
                .unwrap_or_else(|| "http://localhost:8080/v1".to_string()),
            envfile::resolve_api_key(env, env.get("AFI_API_KEY").map(|s| s.as_str())),
            env.get("AFI_MODEL").cloned(),
            None,
            None,
        );
        sources.insert("local".to_string(), src);
        source_order.push("local".to_string());
    }

    // Built-in `together` source.
    if !sources.contains_key("together") {
        let key = envfile::resolve_api_key(env, env.get("TOGETHER_API_KEY").map(|s| s.as_str()));
        if let Some(k) = key {
            if !k.is_empty() {
                let src = Source::new(
                    "together",
                    "https://api.together.xyz/v1".to_string(),
                    Some(k),
                    Some("zai-org/GLM-5.2".to_string()),
                    None,
                    None,
                );
                sources.insert("together".to_string(), src);
                source_order.push("together".to_string());
            }
        }
    }

    // Built-in `openrouter` source.
    if !sources.contains_key("openrouter") {
        let key = envfile::resolve_api_key(env, env.get("OPENROUTER_API_KEY").map(|s| s.as_str()));
        if let Some(k) = key {
            if !k.is_empty() {
                let or_body = parse_extra_body(
                    env.get("AFI_SOURCE_OPENROUTER_EXTRA_BODY")
                        .map(|s| s.as_str()),
                )
                .unwrap_or_else(|| {
                    serde_json::json!({
                        "provider": {
                            "order": ["parasail/fp8"],
                            "allow_fallbacks": false
                        }
                    })
                });
                let app_name = env
                    .get("AFI_SOURCE_OPENROUTER_APP_NAME")
                    .map(|s| s.as_str())
                    .or(Some("Minion"));
                let app_url = env
                    .get("AFI_SOURCE_OPENROUTER_APP_URL")
                    .map(|s| s.as_str())
                    .or(Some("https://github.com/Sentdex/minion"));
                let headers = build_http_headers(app_name, app_url);
                let src = Source::new(
                    "openrouter",
                    "https://openrouter.ai/api/v1".to_string(),
                    Some(k),
                    Some("z-ai/glm-5.2".to_string()),
                    Some(or_body),
                    headers,
                );
                sources.insert("openrouter".to_string(), src);
                source_order.push("openrouter".to_string());
            }
        }
    }

    (sources, source_order)
}

#[allow(dead_code)]
fn _silence_unused() {
    let _ = OnceLock::<()>::new();
}
