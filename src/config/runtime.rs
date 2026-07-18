//! `Runtime` session state, CLI arg parsing, and source discovery.

use std::collections::HashMap;
use std::hash::BuildHasher;
use std::path::Path;

use crate::approval::{apply_approval, approval_display, normalize_approval, ApprovalState};
use crate::envfile;

use super::{build_http_headers, parse_extra_body, Source};

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
/// Hand-rolled for now so tests can pass `["afi", "--source", "zai"]`
/// directly without a clap dependency at test time. Phase 8 may swap this for
/// a clap-based parser; the surface should stay byte-identical.
#[must_use]
pub fn parse_args(args: &[String]) -> ParsedArgs {
    let mut out = ParsedArgs::default();
    let mut saw_sessions = false;
    let mut query: Vec<String> = Vec::new();
    let mut i = 1; // skip argv[0]
    while i < args.len() {
        let a = args[i].as_str();
        if i == 1 && a == "sessions" {
            saw_sessions = true;
        } else if saw_sessions {
            query.push(a.to_string());
        } else if apply_flag(&mut out, a, args.get(i + 1).map(String::as_str)) {
            i += 1;
        }
        i += 1;
    }
    if saw_sessions {
        out.sessions_query = Some(query);
    }
    out
}

/// Apply one flag to `out`. Returns `true` when it consumed the following
/// argument as its value.
fn apply_flag(out: &mut ParsedArgs, flag: &str, value: Option<&str>) -> bool {
    match flag {
        "--yolo" => out.yolo = true,
        "--approval" => return set_opt(&mut out.approval, value),
        "--source" => return set_opt(&mut out.source, value),
        "--session" => return set_opt(&mut out.session, value),
        "--prompt-file" | "-f" => return set_opt(&mut out.prompt_file, value),
        "--resume" | "-r" => {
            // bare --resume, or --resume <target> where target doesn't start
            // with '-' (so `--resume --yolo` doesn't swallow --yolo).
            if let Some(v) = value.filter(|v| !v.starts_with('-')) {
                out.resume = Some(Some(v.to_string()));
                return true;
            }
            out.resume = Some(None);
        }
        _ => {}
    }
    false
}

/// Set `slot` to `value` when present; returns whether a value was consumed.
fn set_opt(slot: &mut Option<String>, value: Option<&str>) -> bool {
    if let Some(v) = value {
        *slot = Some(v.to_string());
        return true;
    }
    false
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
    #[must_use]
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
        let approval = resolve_approval(&env, &parsed);

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
            eprintln!("  \u{2717} unknown source {name:?}");
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
    #[must_use]
    pub fn active_source(&self) -> Option<&Source> {
        self.active.as_ref().and_then(|n| self.sources.get(n))
    }
}

// --- Source discovery --------------------------------------------------------

/// Resolve the starting approval state from env (`AFI_APPROVAL`) then the
/// `--approval` / `--yolo` flags. Unknown levels warn and are ignored.
fn resolve_approval(env: &HashMap<String, String>, parsed: &ParsedArgs) -> ApprovalState {
    let mut approval = ApprovalState::default();
    if let Some(val) = env.get("AFI_APPROVAL").filter(|v| !v.trim().is_empty()) {
        if let Some(kind) = normalize_approval(val) {
            apply_approval(&mut approval, kind, true);
        } else {
            eprintln!(
                "  \u{2717} unknown AFI_APPROVAL={val:?} \
                 (want all|low|medium|high|yolo); prompting for all actions"
            );
        }
    }
    if let Some(level) = &parsed.approval {
        if let Some(kind) = normalize_approval(level) {
            apply_approval(&mut approval, kind, true);
        } else {
            eprintln!(
                "  \u{2717} unknown --approval level {level:?} \
                 (want all|low|medium|high|yolo); keeping {}",
                approval_display(&approval)
            );
        }
    }
    if parsed.yolo {
        // bare --yolo: short-circuit everything, never prompt. Does NOT touch
        // default_approve_level (matches the Python --yolo path).
        approval.yolo = true;
        approval.approve_level = None;
    }
    approval
}

/// Build the sources map + ordered list from `AFI_SOURCE_*` env vars,
/// falling back to a single `local` source from the legacy `AFI_*` vars.
#[must_use]
pub fn discover_sources<S: BuildHasher>(
    env: &HashMap<String, String, S>,
) -> (HashMap<String, Source>, Vec<String>) {
    let mut sources: HashMap<String, Source> = HashMap::new();
    let mut order: Vec<String> = Vec::new();

    for name in source_names(env) {
        if let Some(src) = configured_source(env, &name) {
            sources.insert(name.clone(), src);
            order.push(name);
        }
    }
    if sources.is_empty() {
        add_legacy_source(env, &mut sources, &mut order);
    }
    add_together_source(env, &mut sources, &mut order);
    add_openrouter_source(env, &mut sources, &mut order);

    (sources, order)
}

/// The configured source names: an explicit `AFI_SOURCES` list, else
/// auto-discovered from `AFI_SOURCE_*_BASE_URL` keys (sorted).
fn source_names<S: BuildHasher>(env: &HashMap<String, String, S>) -> Vec<String> {
    if let Some(raw) = env.get("AFI_SOURCES").filter(|r| !r.trim().is_empty()) {
        return raw
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }
    let mut found: Vec<String> = env
        .keys()
        .filter_map(|k| {
            k.strip_prefix("AFI_SOURCE_")
                .and_then(|rest| rest.strip_suffix("_BASE_URL"))
                .map(str::to_lowercase)
        })
        .collect();
    found.sort();
    found
}

/// Build a `Source` for `name` from its `AFI_SOURCE_<NAME>_*` vars, or `None`
/// when no `BASE_URL` is set.
fn configured_source<S: BuildHasher>(
    env: &HashMap<String, String, S>,
    name: &str,
) -> Option<Source> {
    let p = format!("AFI_SOURCE_{}_", name.to_uppercase());
    let base_url = env.get(&format!("{p}BASE_URL"))?.clone();
    let api_key =
        envfile::resolve_api_key(env, env.get(&format!("{p}API_KEY")).map(String::as_str));
    let model = env.get(&format!("{p}MODEL")).cloned();
    let extra_body = parse_extra_body(env.get(&format!("{p}EXTRA_BODY")).map(String::as_str));
    let http_headers = build_http_headers(
        env.get(&format!("{p}APP_NAME")).map(String::as_str),
        env.get(&format!("{p}APP_URL")).map(String::as_str),
    );
    Some(Source::new(
        name,
        base_url,
        api_key,
        model,
        extra_body,
        http_headers,
    ))
}

/// Legacy single-source fallback from the bare `AFI_*` vars.
fn add_legacy_source<S: BuildHasher>(
    env: &HashMap<String, String, S>,
    sources: &mut HashMap<String, Source>,
    order: &mut Vec<String>,
) {
    let src = Source::new(
        "local",
        env.get("AFI_BASE_URL")
            .cloned()
            .unwrap_or_else(|| "http://localhost:8080/v1".to_string()),
        envfile::resolve_api_key(env, env.get("AFI_API_KEY").map(String::as_str)),
        env.get("AFI_MODEL").cloned(),
        None,
        None,
    );
    sources.insert("local".to_string(), src);
    order.push("local".to_string());
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
