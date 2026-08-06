//! Stateful REPL core, independent from terminal rendering.

use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::commands::handle_slash_command;
use super::failure::RunFailure;
use super::report::report_run;
use super::{CommandResult, header};
use crate::approval::ApprovalState;
use crate::cli::session_id_from_args;
use crate::config::{Runtime, Source};
use crate::log::log_event;
use crate::model::client::ReqwestClient;
use crate::model::turn::{LoopRequest, run_model_turn_loop};
use crate::model::usage_totals;
use crate::model::{ModelConfig, TurnOutcome};
use crate::prompt::SYSTEM;
use crate::risk::{HighDefaultClassifier, detect_project_root};
use crate::sessions::{self, new_session_id, safe_title};
use crate::summary::{ErrorKind, RunError};
use crate::term::{MessageKind, UserInterface};

/// Inputs for one model loop shared by REPL, one-shot, and `/recover`.
pub(crate) struct TurnParams<'a> {
    pub config: &'a ModelConfig,
    pub source: &'a Source,
    pub model: &'a str,
    pub approval: &'a ApprovalState,
    pub env: &'a HashMap<String, String>,
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
    let client = ReqwestClient::new();
    let classifier = HighDefaultClassifier;
    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let project_root = detect_project_root(Some(&cwd));
    run_model_turn_loop(
        messages,
        LoopRequest {
            config: params.config,
            client: &client,
            source: params.source,
            model: params.model,
            approval: params.approval,
            classifier: &classifier,
            cwd: &cwd,
            project_root: &project_root,
            env: params.env,
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
        let mut messages = vec![json!({"role": "system", "content": SYSTEM})];
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
        let env = self.rt.env.clone();
        match handle_slash_command(
            input,
            &mut self.rt,
            &mut self.messages,
            &mut self.session_id,
            &env,
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
                self.auto_save(input);
                CoreAction::Continue
            }
        }
    }

    pub(crate) fn shutdown(&mut self, ui: &mut dyn UserInterface) {
        let _ = sessions::write_session(
            &self.dir,
            &self.session_id,
            &mut self.messages,
            Some(&json!({})),
        );
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
            let error = "no active source - use /source to select one".to_string();
            ui.message(MessageKind::Error, error.clone());
            self.failure
                .record_error(RunError::new(error, ErrorKind::Input));
            return;
        };
        let outcome = run_turn_loop(
            &mut self.messages,
            &TurnParams {
                config: &self.config,
                source,
                model,
                approval: &self.rt.approval,
                env: &self.rt.env,
                force_final: false,
                recovery_sampling: false,
            },
            ui,
        )
        .await;
        // Remembered for the whole session, not just this turn: a piped run in CI
        // must not exit 0 because a later turn happened to work.
        self.failure.record(&outcome);
    }

    /// Whether any turn in this session failed outright.
    pub(crate) fn failed(&self) -> bool {
        self.failure.failed()
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
        let meta = safe_title(Some(input), 60).map_or_else(
            || json!({"source": self.rt.active, "model": self.rt.model}),
            |title| {
                let cwd = env::current_dir()
                    .ok()
                    .map(|path| path.to_string_lossy().to_string());
                json!({"title": title, "source": self.rt.active, "model": self.rt.model, "cwd": cwd})
            },
        );
        let _ =
            sessions::write_session(&self.dir, &self.session_id, &mut self.messages, Some(&meta));
    }
}

fn resume_session(
    rt: &mut Runtime,
    dir: &Path,
    ui: &mut dyn UserInterface,
) -> Option<(Vec<Value>, String)> {
    let target = rt.resume.clone()?;
    let sid = if let Some(target) = target {
        session_id_from_args(&["--resume".to_string(), target], &rt.env)?
    } else {
        let Some(summary) = sessions::list_sessions(dir, Some(1), 0, None)
            .first()
            .cloned()
        else {
            ui.message(
                MessageKind::Info,
                "no saved sessions to resume - starting fresh".to_string(),
            );
            return None;
        };
        summary.id
    };
    let data = sessions::load_session(dir, &sid)?;
    let stored = data.get("messages").and_then(Value::as_array)?;
    let mut messages: Vec<Value> = stored
        .iter()
        .filter(|message| message.get("role").and_then(Value::as_str) != Some("system"))
        .cloned()
        .collect();
    messages.insert(0, json!({"role": "system", "content": SYSTEM}));
    if let Some(source) = data.get("source").and_then(Value::as_str) {
        rt.restore_source(Some(source), data.get("model").and_then(Value::as_str));
    }
    ui.message(
        MessageKind::Info,
        format!("↻ resumed session {sid} ({} messages)", messages.len() - 1),
    );
    Some((messages, sid))
}

pub(crate) fn restore_prompt_resume(rt: &mut Runtime) {
    let Some(target) = rt.resume.clone() else {
        return;
    };
    let dir = sessions::sessions_dir(&rt.env);
    let sid = if let Some(target) = target {
        session_id_from_args(&["--resume".to_string(), target], &rt.env)
    } else {
        sessions::list_sessions(&dir, Some(1), 0, None)
            .first()
            .map(|summary| summary.id.clone())
    };
    let Some(data) = sid.and_then(|sid| sessions::load_session(&dir, &sid)) else {
        return;
    };
    if let Some(source) = data.get("source").and_then(Value::as_str) {
        rt.restore_source(Some(source), data.get("model").and_then(Value::as_str));
    }
}

/// Run a single prompt and report whether it succeeded.
///
/// The bool is the process exit status: a one-shot run that printed an HTTP error
/// used to exit 0, so CI treated a failed run as a passing one. An undelivered
/// summary counts the same way - the answer got no further than this process.
pub(crate) async fn run_one_shot_async(
    prompt_file: &str,
    rt: &Runtime,
    ui: &mut dyn UserInterface,
) -> bool {
    // One process is one run, but tests share a process.
    usage_totals::reset();
    let started = Instant::now();
    let mut messages = Vec::new();
    let outcome = one_shot_run(prompt_file, rt, ui, &mut messages).await;
    let reported = report_run(rt, &messages, outcome.as_ref().err(), started.elapsed(), ui);
    outcome.is_ok() && reported
}

/// The run itself. `Err` carries the reason, already reported to the ui.
async fn one_shot_run(
    prompt_file: &str,
    rt: &Runtime,
    ui: &mut dyn UserInterface,
    messages: &mut Vec<Value>,
) -> Result<(), RunError> {
    // Nothing to send, so nothing to blame the provider for: the invocation is
    // what went wrong.
    let prompt = read_prompt_file(prompt_file).map_err(|error| {
        ui.message(MessageKind::Error, error.clone());
        RunError::new(error, ErrorKind::Input)
    })?;
    messages.push(json!({"role": "system", "content": SYSTEM}));
    messages.push(json!({"role": "user", "content": prompt.clone()}));
    log_event("req", &json!({"prompt": prompt, "mode": "one_shot"}));
    let (Some(source), Some(model)) = (rt.active_source(), rt.model.as_ref()) else {
        let error = "no active source - set AFI_BASE_URL and AFI_MODEL".to_string();
        ui.message(MessageKind::Error, error.clone());
        return Err(RunError::new(error, ErrorKind::Input));
    };
    let config = ModelConfig::from_env(&rt.env);
    let outcome = run_turn_loop(
        messages,
        &TurnParams {
            config: &config,
            source,
            model,
            approval: &rt.approval,
            env: &rt.env,
            force_final: false,
            recovery_sampling: false,
        },
        ui,
    )
    .await;
    if let Some(error) = outcome.error() {
        // The same sentence report_client_error printed, so the summary names the
        // failure rather than restating that there was one.
        return Err(error);
    }
    Ok(())
}

fn read_prompt_file(prompt_file: &str) -> Result<String, String> {
    let mut input = String::new();
    if prompt_file == "-" {
        io::stdin()
            .read_to_string(&mut input)
            .map_err(|error| format!("couldn't read stdin: {error}"))?;
    } else {
        input = fs::read_to_string(prompt_file)
            .map_err(|error| format!("couldn't read prompt file {prompt_file:?}: {error}"))?;
    }
    let prompt = input.trim().to_string();
    if prompt.is_empty() {
        Err("prompt file is empty".to_string())
    } else {
        Ok(prompt)
    }
}
