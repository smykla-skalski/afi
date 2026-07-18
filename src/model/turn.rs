//! The model turn: one streamed turn (open stream, process SSE chunks,
//! dispatch tools, return a TURN_* status) and the retry loop that wraps it.
//!
//! The request is awaited under `term::activity::run_during_generation`, which
//! animates the Life spinner and lets Esc interrupt generation; chunk folding,
//! tool dispatch, and reporting live in the `turn_stream`, `turn_dispatch`, and
//! `turn_stats` sibling modules.

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use serde_json::{Value, json};

use crate::approval::ApprovalState;
use crate::config::Source;
use crate::log::log_event;
use crate::model::client::{ChatClient, ClientError, StreamRequest};
use crate::model::recovery::recovery_sampling_opts;
use crate::model::stream::StreamChunk;
use crate::model::turn_finalize::finalize_turn;
use crate::model::turn_stats::handle_reasoning_stall;
use crate::model::turn_stream::{StreamResult, accumulate};
use crate::model::{FINAL_ANSWER_TOOL, FINAL_ANSWER_TOOL_CHOICE, ModelConfig, TURN_DONE, TURN_ESC};
use crate::risk::RiskClassifier;
use crate::term::activity::{Generation, run_during_generation};
use crate::term::interrupt::InterruptWatcher;
use crate::tools;

pub use crate::model::turn_loop::{LoopRequest, run_model_turn_loop};

/// Bundles the parameters for a single model turn.
pub struct TurnRequest<'a> {
    pub config: &'a ModelConfig,
    pub client: &'a dyn ChatClient,
    pub source: &'a Source,
    pub model: &'a str,
    pub approval: &'a ApprovalState,
    pub classifier: &'a dyn RiskClassifier,
    pub cwd: &'a Path,
    pub project_root: &'a Path,
    pub env: &'a HashMap<String, String>,
    pub reasoning_loop_cut_count: u32,
    pub malformed_stream_cut_count: u32,
    pub empty_turn_count: u32,
    pub forced_final: bool,
    pub recovery_sampling: bool,
}

/// Run one streamed model turn. Returns a TURN_* status string.
pub async fn model_turn(messages: &mut Vec<Value>, tr: TurnRequest<'_>) -> String {
    let t0 = Instant::now();
    let (tools_val, tool_choice, max_tokens) = request_params(&tr);
    let extra_body = merged_extra_body(&tr);

    let chunks = match fetch_chunks(
        &tr,
        messages,
        &tools_val,
        tool_choice.as_ref(),
        max_tokens,
        extra_body.as_ref(),
    )
    .await
    {
        Ok(c) => c,
        Err(status) => return status,
    };
    log_event("resp", &json!({"chunks": chunks.len()}));

    let acc = match accumulate(&chunks, tr.config, t0) {
        StreamResult::Done(a) => a,
        StreamResult::ReasoningStall {
            chars,
            reasoning_parts,
        } => {
            return handle_reasoning_stall(
                messages,
                tr.config,
                tr.reasoning_loop_cut_count,
                chars,
                &reasoning_parts,
                tr.forced_final,
            );
        }
    };
    finalize_turn(messages, &tr, acc, t0)
}

/// The forced-final vs normal tools/token limits for the request.
fn request_params(tr: &TurnRequest<'_>) -> (Value, Option<Value>, Option<u32>) {
    let tools_val = if tr.forced_final {
        json!([FINAL_ANSWER_TOOL.clone()])
    } else {
        tools::TOOLS.clone()
    };
    let tool_choice = tr.forced_final.then(|| FINAL_ANSWER_TOOL_CHOICE.clone());
    let max_tokens = if tr.forced_final {
        Some(tr.config.forced_final_max_tokens)
    } else if tr.config.max_completion_tokens > 0 {
        Some(tr.config.max_completion_tokens)
    } else {
        None
    };
    (tools_val, tool_choice, max_tokens)
}

/// The source `extra_body`, merged with the recovery sampling knobs when
/// recovery sampling is active.
fn merged_extra_body(tr: &TurnRequest<'_>) -> Option<Value> {
    let mut extra_body = tr.source.extra_body.clone();
    if !tr.recovery_sampling {
        return extra_body;
    }
    let recovery_opts = recovery_sampling_opts(tr.config);
    if let (Some(reb), Some(eb)) = (recovery_opts.as_object(), extra_body.as_mut()) {
        if let Some(eb_obj) = eb.as_object_mut() {
            for (k, v) in reb {
                eb_obj.insert(k.clone(), v.clone());
            }
        }
    } else if extra_body.is_none() {
        extra_body = Some(recovery_opts);
    }
    extra_body
}

/// Stream the completion, mapping transport errors and Esc-interrupt to a
/// terminal TURN_* status (returned as `Err`).
async fn fetch_chunks(
    tr: &TurnRequest<'_>,
    messages: &mut Vec<Value>,
    tools_val: &Value,
    tool_choice: Option<&Value>,
    max_tokens: Option<u32>,
    extra_body: Option<&Value>,
) -> Result<Vec<StreamChunk>, String> {
    let stream_req = StreamRequest {
        source: tr.source,
        model: tr.model,
        messages: messages.as_slice(),
        tools: Some(tools_val),
        tool_choice,
        max_tokens,
        extra_body,
        recovery_sampling: tr.recovery_sampling,
    };
    log_event(
        "req",
        &json!({"model": tr.model, "stream": true, "forced_final": tr.forced_final, "recovery_sampling": tr.recovery_sampling}),
    );
    let interrupt = InterruptWatcher::new();
    let stream_fut = tr.client.chat_completions_stream(stream_req);
    match run_during_generation(&interrupt, "thinking", stream_fut).await {
        Generation::Completed(Ok(c)) => Ok(c),
        Generation::Completed(Err(ClientError::Connection(msg))) => {
            eprintln!(
                "\x1b[31m  \u{2717} can't reach {} - is the server up?\n    {}\x1b[0m",
                tr.source.base_url, msg
            );
            Err(TURN_DONE.to_string())
        }
        Generation::Completed(Err(ClientError::Http { status, body })) => {
            let body_short = &body[..body.len().min(200)];
            eprintln!("\x1b[31m  \u{2717} HTTP {status}: {body_short}\x1b[0m");
            Err(TURN_DONE.to_string())
        }
        Generation::Completed(Err(ClientError::Parse(msg))) => {
            eprintln!("\x1b[31m  \u{2717} parse error: {msg}\x1b[0m");
            Err(TURN_DONE.to_string())
        }
        Generation::Interrupted => {
            eprintln!("\x1b[33m  \u{21b3} interrupted by Esc\x1b[0m");
            messages.push(json!({"role": "user", "content": "[User pressed Esc to interrupt generation. Acknowledge briefly and wait.]"}));
            Err(TURN_ESC.to_string())
        }
    }
}
