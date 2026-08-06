//! The model turn: one streamed turn (open stream, process SSE chunks,
//! dispatch tools, return a TURN_* status) and the retry loop that wraps it.
//!
//! Frontends receive live chunks through typed UI events. Their cancellation
//! token races both HTTP setup and every stream read.

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use futures::StreamExt;
use serde_json::{Value, json};

use crate::approval::ApprovalState;
use crate::config::Source;
use crate::log::log_event;
use crate::model::client::{ChatClient, ChatCompletionStream, ClientError, StreamRequest};
use crate::model::recovery::recovery_sampling_opts;
use crate::model::turn_finalize::finalize_turn;
use crate::model::turn_stats::handle_reasoning_stall;
use crate::model::turn_stream::{StreamAccumulator, StreamResult};
use crate::model::{
    FINAL_ANSWER_TOOL, FINAL_ANSWER_TOOL_CHOICE, ModelConfig, TURN_ESC, TURN_FAILED,
};
use crate::risk::RiskClassifier;
use crate::term::{MessageKind, UserInterface};
use crate::tools;
use tokio_util::sync::CancellationToken;

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
pub async fn model_turn(
    messages: &mut Vec<Value>,
    tr: TurnRequest<'_>,
    ui: &mut dyn UserInterface,
) -> String {
    let t0 = Instant::now();
    let (tools_val, tool_choice, max_tokens) = request_params(&tr);
    let extra_body = merged_extra_body(&tr);

    let params = FetchParams {
        tools: &tools_val,
        tool_choice: tool_choice.as_ref(),
        max_tokens,
        extra_body: extra_body.as_ref(),
        started: t0,
    };
    let cancel = ui.start_activity("thinking");
    let stream_result = match fetch_stream(&tr, messages, &params, &cancel, ui).await {
        Ok(result) => result,
        Err(status) => return status,
    };

    let acc = match stream_result {
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
                ui,
            );
        }
    };
    finalize_turn(messages, &tr, acc, t0, &cancel, ui)
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
struct FetchParams<'a> {
    tools: &'a Value,
    tool_choice: Option<&'a Value>,
    max_tokens: Option<u32>,
    extra_body: Option<&'a Value>,
    started: Instant,
}

async fn fetch_stream(
    tr: &TurnRequest<'_>,
    messages: &mut Vec<Value>,
    params: &FetchParams<'_>,
    cancel: &CancellationToken,
    ui: &mut dyn UserInterface,
) -> Result<StreamResult, String> {
    let stream_req = StreamRequest {
        source: tr.source,
        model: tr.model,
        messages: messages.as_slice(),
        tools: Some(params.tools),
        tool_choice: params.tool_choice,
        max_tokens: params.max_tokens,
        extra_body: params.extra_body,
        recovery_sampling: tr.recovery_sampling,
    };
    log_event(
        "req",
        &json!({"model": tr.model, "stream": true, "forced_final": tr.forced_final, "recovery_sampling": tr.recovery_sampling}),
    );
    let mut stream = match open_stream(tr, stream_req, cancel).await {
        Ok(stream) => stream,
        Err(StreamOpenError::Cancelled) => {
            ui.stop_activity();
            return Err(interrupt_generation(messages, None, ui));
        }
        Err(StreamOpenError::Client(error)) => {
            ui.stop_activity();
            return Err(report_client_error(error, tr.source, ui));
        }
    };
    let mut accumulator = StreamAccumulator::new();
    let mut chunks = 0usize;
    loop {
        let item = tokio::select! {
            item = stream.next() => item,
            () = cancel.cancelled() => {
                ui.stop_activity();
                return Err(interrupt_generation(messages, Some(accumulator), ui));
            }
        };
        let Some(item) = item else {
            break;
        };
        let chunk = match item {
            Ok(chunk) => chunk,
            Err(error) => {
                ui.stop_activity();
                if preserve_partial(messages, accumulator, ui) {
                    messages.push(json!({"role": "user", "content": "[Runtime note: The previous assistant stream ended before completion. Do not assume the partial answer was complete.]"}));
                }
                return Err(report_client_error(error, tr.source, ui));
            }
        };
        chunks += 1;
        if let Some(result) = accumulator.push(&chunk, tr.config, params.started, ui) {
            ui.stop_activity();
            log_event("resp", &json!({"chunks": chunks, "cut": true}));
            return Ok(result);
        }
    }
    ui.stop_activity();
    log_event("resp", &json!({"chunks": chunks}));
    Ok(accumulator.finish(ui))
}

async fn open_stream(
    tr: &TurnRequest<'_>,
    request: StreamRequest<'_>,
    cancel: &CancellationToken,
) -> Result<ChatCompletionStream, StreamOpenError> {
    tokio::select! {
        result = tr.client.chat_completions_stream(request) => {
            result.map_err(StreamOpenError::Client)
        },
        () = cancel.cancelled() => Err(StreamOpenError::Cancelled),
    }
}

enum StreamOpenError {
    Cancelled,
    Client(ClientError),
}

fn report_client_error(error: ClientError, source: &Source, ui: &mut dyn UserInterface) -> String {
    let message = match error {
        ClientError::Connection(message) => {
            format!(
                "can't reach {} - is the server up?\n{message}",
                source.base_url
            )
        }
        ClientError::Http { status, body } => {
            let body_short: String = body.chars().take(200).collect();
            format!("HTTP {status}: {body_short}")
        }
        ClientError::Parse(message) => format!("parse error: {message}"),
        // Already a complete, actionable sentence - no prefix, and no claim
        // about the server, which was never contacted.
        ClientError::Config(message) => message,
    };
    ui.message(MessageKind::Error, message);
    // Not TURN_DONE: the run failed, and reporting it as done is what made a
    // one-shot exit 0 after printing an HTTP error.
    TURN_FAILED.to_string()
}

fn interrupt_generation(
    messages: &mut Vec<Value>,
    accumulator: Option<StreamAccumulator>,
    ui: &mut dyn UserInterface,
) -> String {
    if let Some(accumulator) = accumulator {
        preserve_partial(messages, accumulator, ui);
    } else {
        ui.finish_stream();
    }
    ui.message(MessageKind::Warning, "interrupted by Esc".to_string());
    messages.push(json!({"role": "user", "content": "[User pressed Esc to interrupt generation. Acknowledge briefly and wait.]"}));
    TURN_ESC.to_string()
}

fn preserve_partial(
    messages: &mut Vec<Value>,
    accumulator: StreamAccumulator,
    ui: &mut dyn UserInterface,
) -> bool {
    let StreamResult::Done(accumulated) = accumulator.finish(ui) else {
        return false;
    };
    let text = accumulated.content_parts.join("");
    if text.is_empty() {
        return false;
    }
    messages.push(json!({"role": "assistant", "content": text}));
    true
}
