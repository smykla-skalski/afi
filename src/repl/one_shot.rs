//! The one-shot path: one prompt, one answer, then the process exits.
//!
//! Apart from [`super::core`] because the two differ in the thing that matters
//! most to everything downstream - whether there is a session afterwards. A REPL
//! turn folds its context on the way out because something will read it next; a
//! one-shot run has no next, so it does not.

use std::fs;
use std::io::{self, Read};
use std::time::Instant;

use serde_json::{Value, json};

use super::core::{Shared, TurnParams, run_turn_loop};
use super::report::report_run;
use crate::config::Runtime;
use crate::log::log_event;
use crate::model::ModelConfig;
use crate::model::client::ReqwestClient;
use crate::summary::{ErrorKind, RunError};
use crate::term::{MessageKind, UserInterface};

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
    messages.push(rt.prompt().message());
    messages.push(json!({"role": "user", "content": prompt.clone()}));
    log_event("req", &json!({"prompt": prompt, "mode": "one_shot"}));
    let (Some(source), Some(model)) = (rt.active_source(), rt.model.as_ref()) else {
        let error = "no active source - set AFI_BASE_URL and AFI_MODEL".to_string();
        ui.message(MessageKind::Error, error.clone());
        return Err(RunError::new(error, ErrorKind::Input));
    };
    let config = ModelConfig::from_env(&rt.env);
    // One turn, so this client caches nothing anyone gets to reuse - which is
    // the shape a one-shot run has anyway.
    let client = ReqwestClient::new();
    let outcome = run_turn_loop(
        messages,
        &TurnParams {
            config: &config,
            prompt: rt.prompt(),
            source,
            model,
            approval: &rt.approval,
            shared: &Shared {
                client: &client,
                env: &rt.env,
            },
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
