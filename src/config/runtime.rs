//! `Runtime` session state and source discovery. Argument parsing lives in
//! `args`.

use std::collections::HashMap;
use std::hash::BuildHasher;
use std::path::{Path, PathBuf};

use crate::approval::{ApprovalState, apply_approval, approval_display, normalize_approval};
use crate::envfile;
use crate::pricing::Pricing;
use crate::summary::{SummaryFormat, summary_path, writable};
use crate::tools::policy::ToolPolicy;

use super::args::{ParsedArgs, parse_args};
use super::builtins::add_builtin_sources;
use super::effort::{self, Effort};
use super::tools::apply_tool_flags;
use super::{Protocol, Source, build_http_headers, parse_extra_body};

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
    /// How to report the run once it finishes. Off unless asked for.
    pub summary: SummaryFormat,
    /// Where to also write that report. Independent of `summary`: a path does
    /// not turn the stdout copy on, and stdout does not stand in for a path.
    pub summary_file: Option<PathBuf>,
    /// Which tools this run may call. Held rather than re-derived because the
    /// header renders it on every frame; `ModelConfig::from_env` reads the same
    /// env vars, so the two cannot disagree.
    pub tool_policy: ToolPolicy,
    /// How hard this run asks the model to think, translated into each source's
    /// own wire format. `None` leaves every source at its endpoint's default.
    pub effort: Option<Effort>,
    /// Flags given wrongly on the command line. See `refusals`.
    pub flag_errors: Vec<String>,
    /// Token rates for the summary's cost, `None` when unset or unusable.
    pub pricing: Option<Pricing>,
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
        apply_tool_flags(&mut env, &parsed);
        let mut flag_errors = parsed.flag_errors;
        let effort = effort::resolve(
            parsed.effort.as_deref(),
            env.get("AFI_EFFORT").map(String::as_str),
        )
        .unwrap_or_else(|refusal| {
            flag_errors.push(refusal);
            None
        });

        let mut rt = Runtime {
            sources,
            source_order,
            active: None,
            model: None,
            approval,
            prompt_file: parsed.prompt_file,
            resume: parsed.resume,
            session: parsed.session,
            // The flag wins over the env var, matching every other setting.
            summary: SummaryFormat::from_value(
                parsed
                    .summary
                    .as_deref()
                    .or_else(|| env.get("AFI_SUMMARY").map(String::as_str)),
            ),
            summary_file: summary_path(
                parsed
                    .summary_file
                    .as_deref()
                    .or_else(|| env.get("AFI_SUMMARY_FILE").map(String::as_str)),
            ),
            tool_policy: ToolPolicy::from_env(
                env.get("AFI_ALLOWED_TOOLS").map(String::as_str),
                env.get("AFI_DISALLOWED_TOOLS").map(String::as_str),
                env.get("AFI_READ_ONLY").map(String::as_str),
            ),
            effort,
            flag_errors,
            // At startup, so a typo is heard about before the run, not after.
            pricing: Pricing::from_env(&env),
            env,
        };

        let start = parsed
            .source
            .or_else(|| rt.env.get("AFI_ACTIVE").cloned())
            .or_else(|| rt.source_order.first().cloned());
        if let Some(name) = start {
            rt.switch_source(&name, None);
        }
        // After the starting source is known, so the one warning it can raise
        // names the source the run will actually use.
        effort::apply_to_sources(&mut rt);

        rt
    }

    /// Swap the active source. Reassigns `active` + `model`. Returns `false`
    /// if the name is unknown.
    ///
    /// `model_override` (optional) pins `model` to a specific id for this
    /// switch - used by `/source <name> <model>` so a multi-model host can be
    /// pointed at any of its models without a config edit. A bare switch (no
    /// override) always falls back to the source's configured default.
    pub fn switch_source(&mut self, name: &str, model_override: Option<&str>) -> bool {
        if !self.sources.contains_key(name) {
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

    /// Why this run must not start, if it must not.
    ///
    /// Every case is a setting whose quiet fallback leaves a finished run that
    /// differs from the one the command line asked for, with nothing downstream
    /// to notice: a wider tool grant than was asked for, or an effort nobody
    /// chose. The summary-file case is checked here, by touching the path,
    /// rather than left to the write at the end of the run: a caller that asked
    /// for a file is not watching stdout for the JSON, and a run that has
    /// already been paid for is a poor moment to learn the directory does not
    /// exist.
    #[must_use]
    pub fn refusals(&self) -> Vec<String> {
        let mut out = self.flag_errors.clone();
        out.extend(self.tool_policy.unknown_names_message());
        out.extend(self.summary_file.as_deref().and_then(|p| writable(p).err()));
        out
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
    add_builtin_sources(env, &mut sources, &mut order);

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
    let protocol = env
        .get(&format!("{p}PROTOCOL"))
        .map_or(Protocol::default(), |raw| Protocol::from_env_value(raw));
    Some(
        Source::new(name, base_url, api_key, model, extra_body, http_headers)
            .with_protocol(protocol),
    )
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
