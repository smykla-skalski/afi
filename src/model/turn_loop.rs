//! The retry loop around a single model turn: it re-runs `model_turn` based on
//! the returned TURN_* status until the turn is DONE or the user escapes, then
//! forces a final answer if the turn budget is exhausted.

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;

use crate::approval::ApprovalState;
use crate::config::Source;
use crate::model::client::ChatClient;
use crate::model::turn::{model_turn, TurnRequest};
use crate::model::{
    ModelConfig, TURN_DONE, TURN_EMPTY, TURN_ESC, TURN_FORCE_FINAL, TURN_STREAM_CUT, TURN_TOOL,
};
use crate::risk::RiskClassifier;

/// Bundles the parameters for the model turn loop.
pub struct LoopRequest<'a> {
    pub config: &'a ModelConfig,
    pub client: &'a dyn ChatClient,
    pub source: &'a Source,
    pub model: &'a str,
    pub approval: &'a ApprovalState,
    pub classifier: &'a dyn RiskClassifier,
    pub cwd: &'a Path,
    pub project_root: &'a Path,
    pub env: &'a HashMap<String, String>,
    pub force_final: bool,
    pub recovery_sampling: bool,
}

/// The model turn loop: retries based on TURN_* status until DONE/ESC.
pub async fn run_model_turn_loop(messages: &mut Vec<Value>, lr: LoopRequest<'_>) {
    let max_turns: u32 = lr
        .env
        .get("AFI_MAX_MODEL_TURNS")
        .and_then(|v| v.parse().ok())
        .unwrap_or(200);
    let mut steps: u32 = 0;
    let mut reasoning_loop_cuts: u32 = 0;
    let mut malformed_stream_cuts: u32 = 0;
    let mut empty_turn_cuts: u32 = 0;
    let mut force_final = lr.force_final;
    let mut recovery_sampling = lr.recovery_sampling;

    while steps < max_turns {
        let status = model_turn(
            messages,
            TurnRequest {
                config: lr.config,
                client: lr.client,
                source: lr.source,
                model: lr.model,
                approval: lr.approval,
                classifier: lr.classifier,
                cwd: lr.cwd,
                project_root: lr.project_root,
                env: lr.env,
                reasoning_loop_cut_count: reasoning_loop_cuts,
                malformed_stream_cut_count: malformed_stream_cuts,
                empty_turn_count: empty_turn_cuts,
                forced_final: force_final,
                recovery_sampling,
            },
        )
        .await;

        force_final = false;
        recovery_sampling = false;

        if status == TURN_DONE || status == TURN_ESC {
            break;
        }
        steps += 1;

        if status == TURN_STREAM_CUT {
            malformed_stream_cuts += 1;
            recovery_sampling = true;
        } else if status == TURN_EMPTY {
            empty_turn_cuts += 1;
            recovery_sampling = true;
        } else if status == TURN_FORCE_FINAL {
            reasoning_loop_cuts += 1;
            empty_turn_cuts = 0;
            force_final = true;
            recovery_sampling = true;
        } else if status == TURN_TOOL {
            malformed_stream_cuts = 0;
            empty_turn_cuts = 0;
        }
    }

    if steps >= max_turns && !force_final {
        eprintln!(
            "\x1b[33m  \u{26a0} MODEL TURN LIMIT ({}) - forcing final\x1b[0m",
            max_turns
        );
        let _ = model_turn(
            messages,
            TurnRequest {
                config: lr.config,
                client: lr.client,
                source: lr.source,
                model: lr.model,
                approval: lr.approval,
                classifier: lr.classifier,
                cwd: lr.cwd,
                project_root: lr.project_root,
                env: lr.env,
                reasoning_loop_cut_count: reasoning_loop_cuts,
                malformed_stream_cut_count: malformed_stream_cuts,
                empty_turn_count: empty_turn_cuts,
                forced_final: true,
                recovery_sampling,
            },
        )
        .await;
    }
}
