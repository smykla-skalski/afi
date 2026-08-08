//! `Runtime` session state. Source discovery lives in `discovery`, argument
//! parsing in `args`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::approval::{ApprovalState, starting_approval};
use crate::envfile;
use crate::pricing::Pricing;
use crate::summary::{ErrorKind, RunError, SummaryFormat, summary_path, writable};
use crate::tools::policy::ToolPolicy;

use super::Source;
use super::args::parse_args;
use super::effort;
use super::file::{FileSettings, config_files};
use super::sources::discover_sources;
use super::system_prompt::{self, SystemPrompt};
use super::tools::apply_tool_flags;
use super::window;

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
    /// Flags given wrongly on the command line, each with its kind. See
    /// `refusals`.
    /// Why the run must not start, in the order it is reported. A config file
    /// that would not read comes first, because every setting that then looks
    /// unset is explained by it; the flags follow. Not only flags, despite the
    /// name - an unusable `AFI_EFFORT` lands here too.
    pub flag_errors: Vec<RunError>,
    /// Token rates for the summary's cost, `None` when unset or unusable.
    pub pricing: Option<Pricing>,
    /// The context window `--context-window` set for this run, if it was given.
    /// It outranks every configured value, on every source - see
    /// `window::resolve` - because it is the one figure typed for this run rather
    /// than stored for all of them.
    pub context_window: Option<u64>,
    /// The system prompt every turn of this run sends, resolved once here so a
    /// file named on the command line is read before the run is paid for rather
    /// than on the first request. The project's own instruction files are read
    /// here too, into the same result - see [`super::instructions`].
    ///
    /// Held as the failure rather than as a fallback, so nothing downstream can
    /// send the built-in text to a run that named a file of its own. `refusals`
    /// is what turns the failure into an exit.
    pub system_prompt: Result<SystemPrompt, String>,
}

impl Runtime {
    /// The env a run resolves its settings from, and what a config file said.
    ///
    /// The process environment first, then the env file, then the config files -
    /// each filling only the gaps the last left. The order is what lets a config
    /// file live under an `AFI_HOME` that only the env file names.
    ///
    /// Separate from [`Self::build_resolved`] because the map is wanted before
    /// there is a runtime: `afi sessions` answers without building one, and it
    /// resolves its directory from `AFI_SESSIONS_DIR` and `AFI_HOME`, which a
    /// config file can set. Reading the files after that point would list one
    /// directory while the run saved into another.
    ///
    /// This is the one entry point that reads paths nobody passed in, which is
    /// why [`Self::build`] is not it - a test that wants a hermetic runtime calls
    /// that one and gets no file it did not name.
    #[must_use]
    pub fn resolve_env(
        args: &[String],
        mut env: HashMap<String, String>,
    ) -> (HashMap<String, String>, FileSettings) {
        let env_file = env
            .get("AFI_ENV_FILE")
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|home| home.join(".env")));
        if let Some(path) = &env_file {
            envfile::load_into(&mut env, path);
        }
        // A command line that is already refused gets no config file. The run
        // cannot start either way, and reading the default when `--config` was
        // given wrongly would report a file's problems ahead of the flag that was
        // typed wrong - describing a file nobody named.
        let parsed = parse_args(args);
        let files = if parsed.flag_errors.is_empty() {
            config_files(parsed.config.as_deref(), &env, None)
        } else {
            Vec::new()
        };
        let settings = FileSettings::load(&files);
        settings.apply_to(&mut env);
        (env, settings)
    }

    /// Build a fresh runtime from argv, an env map, and an optional env file,
    /// reading no config file.
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
        Self::build_resolved(args, env, &FileSettings::default())
    }

    /// Build from an env and the config settings that belong with it, from
    /// [`Self::resolve_env`] or from [`FileSettings::load`] directly.
    ///
    /// `settings` is carried rather than re-read so the refusals it found are
    /// reported by [`Self::refusals`] whether or not the caller checked them
    /// itself. Applying it here rather than in the caller is what makes the order
    /// flag, then variable, then file: the file only fills gaps, and
    /// `apply_tool_flags` below overwrites. An env `resolve_env` already applied
    /// is unchanged by the second pass, so both callers are safe.
    #[must_use]
    pub fn build_resolved(
        args: &[String],
        mut env: HashMap<String, String>,
        settings: &FileSettings,
    ) -> Self {
        settings.apply_to(&mut env);
        let (sources, source_order) = discover_sources(&env);
        let parsed = parse_args(args);
        let approval = starting_approval(
            env.get("AFI_APPROVAL").map(String::as_str),
            parsed.approval.as_deref(),
            parsed.yolo,
        );
        apply_tool_flags(&mut env, &parsed);
        // Ahead of the struct below, and ahead of the moves that follow, because
        // it wants the whole of `parsed` and the env after the flags landed in it.
        let system_prompt = system_prompt::for_run(&parsed, &env);
        // The config file first: `refusals` reports in this order, and a file
        // nobody could read explains every setting that then looks unset.
        //
        // `Input`: the invocation named settings this run cannot use, and
        // retrying it lands in the same place.
        let mut flag_errors: Vec<RunError> = settings
            .refusals()
            .iter()
            .map(|why| RunError::new(why.clone(), ErrorKind::Input))
            .collect();
        flag_errors.extend(parsed.flag_errors);
        let context_window = window::from_flag(parsed.context_window.as_deref(), &mut flag_errors);
        let effort = effort::resolve(
            parsed.effort.as_deref(),
            env.get("AFI_EFFORT").map(String::as_str),
        )
        .unwrap_or_else(|refusal| {
            // `Input`: an effort level afi cannot use is the invocation naming
            // something this source has no answer for, and retrying it lands in
            // the same place.
            flag_errors.push(RunError::new(refusal, ErrorKind::Input));
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
            flag_errors,
            // At startup, so a typo is heard about before the run, not after.
            pricing: Pricing::from_env(&env),
            context_window,
            system_prompt,
            env,
        };

        let start = parsed
            .source
            .or_else(|| rt.env.get("AFI_ACTIVE").cloned())
            .or_else(|| rt.default_source());
        if let Some(name) = start {
            rt.switch_source(&name, None);
        }
        // After the starting source is known, so the one warning it can raise
        // names the source the run will actually use. Passed rather than held on
        // the runtime: this is the level that was asked for, and each source
        // caps it separately, so the only honest answer to "what effort did the
        // requests carry" is `Source::resolved_effort`.
        effort::apply_to_sources(&mut rt, effort);

        rt
    }

    /// The source to start on when nothing named one: the first in
    /// `AFI_SOURCES` order that can actually be used, else the first outright.
    ///
    /// Skipping the unusable ones matters because a source that cannot be used
    /// refuses the whole run, and the startup default is a guess afi makes
    /// rather than an instruction it was given. A Bedrock source whose Region
    /// and keys are not exported - an ordinary shell before `aws sso login` -
    /// sorts ahead of `local` on name alone, and without this it would take
    /// every other configured source down with it.
    ///
    /// An *explicit* `--source` or `AFI_ACTIVE` is never second-guessed: asking
    /// for a source that cannot sign is answered with the refusal naming what
    /// is missing, which is the whole point of that check.
    fn default_source(&self) -> Option<String> {
        self.source_order
            .iter()
            .find(|name| {
                self.sources
                    .get(*name)
                    .is_some_and(|source| source.config_error().is_none())
            })
            .or_else(|| self.source_order.first())
            .cloned()
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
        // Resolved here rather than at discovery because the window belongs to
        // the *model*, and this is where the model this source will use is
        // finally known - `/source zai glm-4.6` pins one the source itself never
        // named. See `window::resolve` for the order.
        let window = window::resolve(&self.env, self.context_window, name, &model);
        self.model = Some(model);
        if let Some(s) = self.sources.get_mut(name) {
            s.context_window = window;
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
    /// to notice: a wider tool grant than was asked for, an effort nobody chose,
    /// afi's own prompt in place of the instructions the run was handed, or a
    /// config file whose settings are all absent. The
    /// summary-file case is checked here, by touching the path, rather than left
    /// to the write at the end of the run: a caller that asked for a file is not
    /// watching stdout for the JSON, and a run that has already been paid for is
    /// a poor moment to learn the directory does not exist.
    ///
    /// Each refusal carries the kind the summary reports, decided here where the
    /// reason is known. Deriving it afterwards from which field was non-empty
    /// would put the caller's classification back at one remove from the thing
    /// being classified, which is what `error_kind` exists to end.
    ///
    /// Only the *active* source is checked for a credential it cannot assemble.
    /// A half-configured source nobody switches to costs the run nothing, and
    /// refusing to start over one would make an unused `AWS_ACCESS_KEY_ID` in
    /// the shell enough to block every run.
    #[must_use]
    pub fn refusals(&self) -> Vec<RunError> {
        let mut out = self.flag_errors.clone();
        out.extend(
            self.tool_policy
                .unknown_names_message()
                .map(|m| RunError::new(m, ErrorKind::Policy)),
        );
        out.extend(
            self.summary_file
                .as_deref()
                .and_then(|p| writable(p).err())
                .map(|m| RunError::new(m, ErrorKind::Input)),
        );
        // `Input`: the invocation named a prompt this run cannot use, and
        // retrying it lands in the same place.
        out.extend(
            self.system_prompt
                .as_ref()
                .err()
                .map(|m| RunError::new(m.clone(), ErrorKind::Input)),
        );
        // `Auth`: the source the run starts on cannot assemble a credential, and
        // no retry assembles one either.
        out.extend(
            self.active_source()
                .and_then(Source::config_error)
                .map(|m| RunError::new(m, ErrorKind::Auth)),
        );
        out
    }

    /// The system prompt every turn of this run sends.
    ///
    /// The only place the unresolvable case is answered. A run whose configured
    /// prompt failed has already been stopped by `refusals`, so the built-in
    /// prompt here is unreachable from the binary; it is the answer rather than a
    /// panic for a library caller who builds a `Runtime` and skips the check.
    /// Answering it once matters more than which answer it is - the enforcing and
    /// reporting halves of `--read-only` once disagreed exactly this way.
    #[must_use]
    pub fn prompt(&self) -> &SystemPrompt {
        self.system_prompt
            .as_ref()
            .unwrap_or_else(|_| system_prompt::builtin())
    }
}
