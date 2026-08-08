//! Stateful REPL core, independent from terminal rendering.

use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};

use super::commands::handle_slash_command;
use super::failure::RunFailure;
use super::report::report_run;
use super::resume::resume_session;
use super::{CommandResult, NO_ACTIVE_SOURCE, header};
use crate::approval::ApprovalState;
use crate::config::{Runtime, Source, SystemPrompt, nested};
use crate::model::client::ReqwestClient;
use crate::model::compress::{AutoCompress, fold_after_turn};
use crate::model::turn::{LoopRequest, run_model_turn_loop};
use crate::model::{ModelConfig, TurnOutcome};
use crate::risk::{HighDefaultClassifier, detect_project_root};
use crate::sessions::{self, new_session_id, safe_title};
use crate::summary::{ErrorKind, RunError};
use crate::term::{MessageKind, UserInterface};

/// What a turn borrows from the session instead of building for itself.
///
/// The HTTP client is here for its caches. A federated source assumes its AWS
/// role - or exchanges its Anthropic token - and holds the credential until it
/// nears expiry, which only means anything while the client outlives the turn
/// that minted it. Built per turn, as it was, every turn opened with a round
/// trip to the OIDC endpoint and another to STS, and left a `CloudTrail` entry
/// behind for a credential the previous turn had already paid for. On a busy
/// account that is also afi throttling itself.
pub(crate) struct Shared<'a> {
    pub client: &'a ReqwestClient,
    /// The merged environment the run was configured from, which is not the
    /// process environment - nothing copies `~/.env` or `AFI_ENV_FILE` into that
    /// one.
    pub env: &'a HashMap<String, String>,
}

/// Inputs for one model loop shared by REPL, one-shot, and `/recover`.
pub(crate) struct TurnParams<'a> {
    pub config: &'a ModelConfig,
    /// The prompt this run sends, carried for what it knows about the project
    /// instructions already loaded - see the subtree loader armed below.
    pub prompt: &'a SystemPrompt,
    pub source: &'a Source,
    pub model: &'a str,
    pub approval: &'a ApprovalState,
    pub shared: &'a Shared<'a>,
    pub force_final: bool,
    pub recovery_sampling: bool,
}

/// Run one model loop without owning terminal/runtime lifecycle.
/// Returns the terminal outcome of the run.
pub(crate) async fn run_turn_loop(
    messages: &mut Vec<Value>,
    params: &TurnParams<'_>,
    ui: &mut dyn UserInterface,
) -> TurnOutcome {
    let classifier = HighDefaultClassifier;
    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let project_root = detect_project_root(Some(&cwd));
    // The one funnel every path runs through - REPL, one-shot, `/recover` - and it
    // already resolved the directory the loader measures the subtree from. Arming
    // twice is a no-op, so the per-turn call cannot reset what the session has read.
    nested::arm(params.prompt, &cwd);
    run_model_turn_loop(
        messages,
        LoopRequest {
            config: params.config,
            client: params.shared.client,
            source: params.source,
            model: params.model,
            approval: params.approval,
            classifier: &classifier,
            cwd: &cwd,
            project_root: &project_root,
            env: params.shared.env,
            force_final: params.force_final,
            recovery_sampling: params.recovery_sampling,
        },
        ui,
    )
    .await
}

pub(crate) struct ReplCore {
    rt: Runtime,
    config: ModelConfig,
    dir: PathBuf,
    session_id: String,
    messages: Vec<Value>,
    /// One client for the whole session, so the credential caches on it are
    /// worth having. See [`Shared`].
    client: ReqwestClient,
    /// Set by any turn that failed outright, so the session's exit code and
    /// summary reflect the whole run rather than only its last turn.
    failure: RunFailure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CoreAction {
    Continue,
    Quit,
}

impl CoreAction {
    pub(crate) fn should_quit(self) -> bool {
        self == Self::Quit
    }
}

impl ReplCore {
    pub(crate) fn new(mut rt: Runtime, ui: &mut dyn UserInterface) -> Self {
        let env = rt.env.clone();
        let dir = sessions::sessions_dir(&env);
        let mut session_id = rt.session.clone().unwrap_or_else(new_session_id);
        let mut messages = vec![rt.prompt().message()];
        if let Some((resumed, sid)) = resume_session(&mut rt, &dir, ui) {
            messages = resumed;
            session_id = sid;
        }
        ui.header(header(&rt));
        Self {
            config: ModelConfig::from_env(&env),
            rt,
            dir,
            session_id,
            messages,
            client: ReqwestClient::new(),
            failure: RunFailure::default(),
        }
    }

    pub(crate) async fn handle_input(
        &mut self,
        input: &str,
        ui: &mut dyn UserInterface,
    ) -> CoreAction {
        let input = input.trim();
        if input.is_empty() {
            return CoreAction::Continue;
        }
        // Cloned because the commands take the runtime mutably, and the client is
        // borrowed alongside it - a disjoint field, so the two do not collide.
        let env = self.rt.env.clone();
        let shared = Shared {
            client: &self.client,
            env: &env,
        };
        match handle_slash_command(
            input,
            &mut self.rt,
            &mut self.messages,
            &mut self.session_id,
            &shared,
            ui,
            &mut self.failure,
        )
        .await
        {
            CommandResult::Quit => {
                self.shutdown(ui);
                CoreAction::Quit
            }
            CommandResult::Continue => CoreAction::Continue,
            CommandResult::NotACommand => {
                self.messages
                    .push(json!({"role": "user", "content": input}));
                self.run_user_turn(ui).await;
                // A turn can load a subtree file, so the `instructions:` segment would
                // otherwise stay frozen at the startup count.
                ui.header(header(&self.rt));
                self.auto_save(input);
                CoreAction::Continue
            }
        }
    }

    pub(crate) fn shutdown(&mut self, ui: &mut dyn UserInterface) {
        let meta = self.session_meta(&Value::Null);
        let _ =
            sessions::write_session(&self.dir, &self.session_id, &mut self.messages, Some(&meta));
        ui.message(MessageKind::Info, self.resume_hint());
    }

    pub(crate) fn resume_hint(&self) -> String {
        format!("resume with: afi --resume {}", self.session_id)
    }

    pub(crate) fn into_runtime(self) -> Runtime {
        self.rt
    }

    async fn run_user_turn(&mut self, ui: &mut dyn UserInterface) {
        let (Some(source), Some(model)) = (self.rt.active_source(), self.rt.model.as_ref()) else {
            // A turn that never ran is still a turn that never answered. This used
            // to return quietly, so a piped run with nothing configured printed the
            // error, reported ok:true, and exited 0 - CI read that as a pass.
            let error = NO_ACTIVE_SOURCE.to_string();
            ui.message(MessageKind::Error, error.clone());
            self.failure
                .record_error(RunError::new(error, ErrorKind::Input));
            return;
        };
        let outcome = run_turn_loop(
            &mut self.messages,
            &TurnParams {
                config: &self.config,
                prompt: self.rt.prompt(),
                source,
                model,
                approval: &self.rt.approval,
                shared: &Shared {
                    client: &self.client,
                    env: &self.rt.env,
                },
                force_final: false,
                recovery_sampling: false,
            },
            ui,
        )
        .await;
        // The loop folds between its own requests; this is the other half, and it
        // belongs here because only the session knows there is a next message.
        // Without it a question-and-answer session - one turn per message, so no
        // *between* at all - would grow until the provider refused it.
        // `one_shot_run` has no next message, so it does not do this.
        let ac = AutoCompress {
            client: &self.client,
            source,
            model,
            percent: self.config.autocompress_percent,
            context_window: source.context_window,
        };
        fold_after_turn(&mut self.messages, &outcome, &ac, ui).await;
        // Remembered for the whole session, not just this turn: a piped run in CI
        // must not exit 0 because a later turn happened to work.
        self.failure.record(&outcome);
    }

    /// Whether any turn in this session failed outright.
    pub(crate) fn failed(&self) -> bool {
        self.failure.error().is_some()
    }

    /// Record a failure the session hit outside a turn, so the summary reports it
    /// rather than the run ending quietly.
    pub(crate) fn record_error(&mut self, error: RunError) {
        self.failure.record_error(error);
    }

    /// Report the run, if a report was asked for. Returns whether it was
    /// delivered - see `report_run`.
    pub(crate) fn report(&self, elapsed: Duration, ui: &mut dyn UserInterface) -> bool {
        report_run(&self.rt, &self.messages, self.failure.error(), elapsed, ui)
    }

    fn auto_save(&mut self, input: &str) {
        let title = safe_title(Some(input), 60);
        let cwd = title.as_ref().and_then(|_| {
            env::current_dir()
                .ok()
                .map(|path| path.to_string_lossy().to_string())
        });
        let meta = self.session_meta(&json!({"title": title, "cwd": cwd}));
        let _ =
            sessions::write_session(&self.dir, &self.session_id, &mut self.messages, Some(&meta));
    }

    /// What a saved session records besides its messages.
    ///
    /// One place, because two of them disagreed: the subtree files this run sent have
    /// to be recorded for a resume to know what the model has already been told, and a
    /// save that left them out would hand the next run an empty answer. Null values are
    /// dropped by the store, so an untitled save leaves an earlier title standing.
    fn session_meta(&self, extra: &Value) -> Value {
        let mut meta = json!({
            "source": self.rt.active,
            "model": self.rt.model,
            "instructions": nested::in_history(),
        });
        if let (Some(meta), Some(extra)) = (meta.as_object_mut(), extra.as_object()) {
            for (key, value) in extra {
                meta.insert(key.clone(), value.clone());
            }
        }
        meta
    }
}
