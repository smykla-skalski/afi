//! Post-stream finalization for a model turn: print the stats footer, then run
//! the reasoning-stall / forced-final / tool-dispatch / empty-turn phases in
//! order and return the first terminal outcome.
//!
//! A phase that gives up returns a failure rather than `TURN_DONE`. Several of
//! them used to report a turn as finished after printing an error and pushing no
//! answer, which left a run that produced nothing exiting 0 with `ok: true`.

use std::collections::HashMap;
use std::time::Instant;

use serde_json::{Value, json};

use crate::model::client::THINKING_HISTORY_KEY;
use crate::model::recovery::{
    EMPTY_TURN_NUDGE, FORCED_FINAL_NUDGE, last_is_dangling_tool, nudge_current_user_turn,
};
use crate::model::stream::normalize_usage;
use crate::model::turn::TurnRequest;
use crate::model::turn_dispatch::{
    DispatchArgs, ToolCallAccum, ToolRunOutcome, dispatch_structured, dispatch_text,
    order_tool_calls,
};
use crate::model::turn_stats::{TurnStats, handle_reasoning_stall, print_stats_footer};
use crate::model::turn_stream::Accumulated;
use crate::model::usage_totals;
use crate::model::{
    TURN_DONE, TURN_EMPTY, TURN_ESC, TURN_FORCE_FINAL, TURN_STREAM_CUT, TURN_TOOL, TurnOutcome,
};
use crate::summary::{ErrorKind, RunError};
use crate::term::{MessageKind, StreamKind, UserInterface};
use crate::tools::protocol::parse_text_calls;
use tokio_util::sync::CancellationToken;

/// Post-stream: print stats, then run the reasoning-stall / forced-final /
/// tool-dispatch / empty-turn phases in order, returning the first terminal
/// outcome.
pub(crate) fn finalize_turn(
    messages: &mut Vec<Value>,
    tr: &TurnRequest<'_>,
    acc: Accumulated,
    t0: Instant,
    cancel: &CancellationToken,
    ui: &mut dyn UserInterface,
) -> TurnOutcome {
    let Accumulated {
        content_parts,
        tool_calls,
        reasoning_parts,
        thinking_blocks,
        usage,
        timings,
        finish_reasons,
        streamed_chars,
        reasoning_only_chars,
        t_first,
    } = acc;
    let text = content_parts.join("");
    let elapsed = t0.elapsed().as_secs_f64();
    let prompt_tokens = usage
        .as_ref()
        .map(|u| u.prompt_tokens)
        .or_else(|| timings.as_ref().map(|t| t.prompt_n))
        .unwrap_or(0);
    let completion_tokens = usage
        .as_ref()
        .map(|u| u.completion_tokens)
        .or_else(|| timings.as_ref().map(|t| t.predicted_n))
        .unwrap_or(0);
    print_stats_footer(
        &TurnStats {
            prompt_tokens,
            completion_tokens,
            elapsed,
            t_first,
            streamed_chars,
            text: &text,
            tool_calls: &tool_calls,
        },
        ui,
    );
    // Fold this turn into the run totals. The result used to be dropped here, so
    // the cache and reasoning split was computed and thrown away on every
    // provider, leaving nothing for a run report to draw on.
    if let Some(normalized) = normalize_usage(usage.as_ref(), timings.as_ref(), streamed_chars) {
        usage_totals::record(tr.model, &normalized);
    }

    if reasoning_only_chars > 0
        && text.trim().is_empty()
        && tool_calls.is_empty()
        && !tr.forced_final
    {
        return handle_reasoning_stall(
            messages,
            tr.config,
            tr.reasoning_loop_cut_count,
            reasoning_only_chars,
            &reasoning_parts,
            tr.forced_final,
            ui,
        );
    }
    if let Some(outcome) =
        forced_final_result(messages, tr, &tool_calls, &text, &finish_reasons, ui)
    {
        return outcome;
    }
    let structured = StructuredTurn {
        tool_calls: &tool_calls,
        text: &text,
        thinking_blocks: &thinking_blocks,
    };
    if let Some(outcome) = run_structured_tools(messages, tr, &structured, cancel, ui) {
        return outcome;
    }
    if let Some(outcome) = run_text_tools(messages, tr, &text, cancel, ui) {
        return outcome;
    }
    handle_empty_or_final(messages, tr, &text, ui)
}

/// The forced-final answer / token-limit outcomes, if `forced_final` is set.
fn forced_final_result(
    messages: &mut Vec<Value>,
    tr: &TurnRequest<'_>,
    tool_calls: &HashMap<u32, ToolCallAccum>,
    text: &str,
    finish_reasons: &[String],
    ui: &mut dyn UserInterface,
) -> Option<TurnOutcome> {
    if tr.forced_final && !tool_calls.is_empty() {
        return Some(emit_forced_final(messages, tool_calls, ui));
    }
    if tr.forced_final
        && !text.trim().is_empty()
        && finish_reasons.iter().any(|f| f.contains("length"))
    {
        ui.message(
            MessageKind::Warning,
            "FORCED FINAL HIT TOKEN LIMIT - saved partial".to_string(),
        );
        messages.push(json!({"role": "assistant", "content": format!("{}\n\n[Truncated by token limit before completion.]", text.trim_end())}));
        // A partial answer is still an answer, and it is labelled as partial in
        // the transcript, so this one stays a finished turn.
        return Some(TurnOutcome::new(TURN_DONE));
    }
    None
}

/// Extract the `final_answer` tool call, or report the miss as a failed turn.
///
/// Both misses used to report DONE, so a forced final that answered with nothing -
/// or with another tool call - looked like a completed run.
fn emit_forced_final(
    messages: &mut Vec<Value>,
    tool_calls: &HashMap<u32, ToolCallAccum>,
    ui: &mut dyn UserInterface,
) -> TurnOutcome {
    let ordered = order_tool_calls(tool_calls);
    for c in &ordered {
        if c.name.as_deref() != Some("final_answer") {
            continue;
        }
        let args: Value = serde_json::from_str(&c.args).unwrap_or(json!({}));
        let answer = args
            .get("answer")
            .and_then(|a| a.as_str())
            .unwrap_or("")
            .trim();
        if answer.is_empty() {
            return no_answer("FORCED FINAL ANSWER EMPTY".to_string(), ui);
        }
        ui.stream(StreamKind::Assistant, answer.to_string());
        ui.finish_stream();
        messages.push(json!({"role": "assistant", "content": answer}));
        return TurnOutcome::new(TURN_DONE);
    }
    let names: Vec<&str> = ordered
        .iter()
        .map(|c| c.name.as_deref().unwrap_or("tool"))
        .collect();
    no_answer(
        format!("FORCED FINAL FAILED - model emitted {}", names.join(", ")),
        ui,
    )
}

/// Report a turn that ended with no answer to show.
///
/// The sentence goes to the ui and to the run summary at once, so the log and the
/// JSON name the same thing. `answer` in the summary is whatever the run last
/// managed to say, which on one of these is an earlier turn's text - `ok: false` is
/// what keeps a workflow from posting it.
fn no_answer(message: String, ui: &mut dyn UserInterface) -> TurnOutcome {
    ui.message(MessageKind::Error, message.clone());
    TurnOutcome::failed(RunError::new(message, ErrorKind::NoAnswer))
}

/// The parts of a streamed turn that a structured tool dispatch needs.
struct StructuredTurn<'a> {
    tool_calls: &'a HashMap<u32, ToolCallAccum>,
    text: &'a str,
    thinking_blocks: &'a [Value],
}

/// Push the assistant turn that carries the structured tool calls.
///
/// Any thinking blocks ride along under `THINKING_HISTORY_KEY`. This is the
/// turn that needs them: Anthropic requires a thinking block that accompanied a
/// `tool_use` to be echoed back verbatim on the request carrying that tool's
/// result, and the `OpenAI`-shape fields alone cannot express one.
fn push_assistant_tool_calls(
    messages: &mut Vec<Value>,
    ordered: &[ToolCallAccum],
    turn: &StructuredTurn<'_>,
) {
    let tool_calls_json: Vec<Value> = ordered.iter().map(|c| json!({"id": c.id.clone().unwrap_or_default(), "type": "function", "function": {"name": c.name.clone().unwrap_or_default(), "arguments": c.args.clone()}})).collect();
    let content = if turn.text.trim().is_empty() {
        Value::Null
    } else {
        json!(turn.text)
    };
    let mut message =
        json!({"role": "assistant", "content": content, "tool_calls": tool_calls_json});
    if !turn.thinking_blocks.is_empty() {
        message[THINKING_HISTORY_KEY] = Value::Array(turn.thinking_blocks.to_vec());
    }
    messages.push(message);
}

/// Warn about (and either give up or retry) a malformed tool-call payload.
fn malformed_tool_retry(
    messages: &mut Vec<Value>,
    tr: &TurnRequest<'_>,
    ordered: &[ToolCallAccum],
    idx: usize,
    err: &str,
    ui: &mut dyn UserInterface,
) -> TurnOutcome {
    let name = ordered[idx].name.as_deref().unwrap_or("tool");
    let retry_limit = tr.config.malformed_stream_retry_limit;
    if tr.malformed_stream_cut_count >= retry_limit {
        // Out of recoveries with nothing dispatched and nothing pushed: the turn
        // produced no answer, whatever the model meant to call.
        return no_answer(
            format!(
                "malformed tool call after {} recoveries",
                tr.malformed_stream_cut_count
            ),
            ui,
        );
    }
    ui.message(
        MessageKind::Warning,
        format!(
            "MALFORMED TOOL CALL - {name} args invalid ({err}); retrying ({}/{retry_limit})",
            tr.malformed_stream_cut_count + 1
        ),
    );
    nudge_current_user_turn(
        messages,
        "Your previous tool call had malformed JSON arguments. Retry with valid arguments.",
    );
    TurnOutcome::new(TURN_STREAM_CUT)
}

/// Dispatch structured (`tool_calls`) calls, if any. `None` when there are none.
fn run_structured_tools(
    messages: &mut Vec<Value>,
    tr: &TurnRequest<'_>,
    turn: &StructuredTurn<'_>,
    cancel: &CancellationToken,
    ui: &mut dyn UserInterface,
) -> Option<TurnOutcome> {
    if turn.tool_calls.is_empty() {
        return None;
    }
    let ordered = order_tool_calls(turn.tool_calls);
    let mut parsed_args: Vec<Value> = Vec::new();
    for (i, c) in ordered.iter().enumerate() {
        match serde_json::from_str::<Value>(&c.args) {
            Ok(v) => parsed_args.push(v),
            Err(e) => {
                return Some(malformed_tool_retry(
                    messages,
                    tr,
                    &ordered,
                    i,
                    &e.to_string(),
                    ui,
                ));
            }
        }
    }
    push_assistant_tool_calls(messages, &ordered, turn);
    let mut da = DispatchArgs {
        approval: tr.approval,
        classifier: tr.classifier,
        cwd: tr.cwd,
        project_root: tr.project_root,
        env: tr.env,
        config: tr.config,
        cancel,
        ui,
    };
    match dispatch_structured(
        messages,
        &ordered,
        &parsed_args,
        &mut da,
        tr.config.tool_result_chars,
    ) {
        ToolRunOutcome::Escaped(action) => {
            da.ui.message(
                MessageKind::Warning,
                format!("tool execution cancelled: {action}"),
            );
            messages.push(json!({"role": "user", "content": "[User pressed Esc to cancel tool execution. Acknowledge briefly and wait.]"}));
            Some(TurnOutcome::new(TURN_ESC))
        }
        ToolRunOutcome::Ran => Some(TurnOutcome::new(TURN_TOOL)),
    }
}

/// Dispatch text-protocol tool calls, if any. `None` when there are none.
fn run_text_tools(
    messages: &mut Vec<Value>,
    tr: &TurnRequest<'_>,
    text: &str,
    cancel: &CancellationToken,
    ui: &mut dyn UserInterface,
) -> Option<TurnOutcome> {
    let calls = parse_text_calls(text);
    if calls.is_empty() {
        return None;
    }
    messages.push(json!({"role": "assistant", "content": text}));
    let mut da = DispatchArgs {
        approval: tr.approval,
        classifier: tr.classifier,
        cwd: tr.cwd,
        project_root: tr.project_root,
        env: tr.env,
        config: tr.config,
        cancel,
        ui,
    };
    match dispatch_text(messages, &calls, &mut da) {
        ToolRunOutcome::Escaped(action) => {
            da.ui.message(
                MessageKind::Warning,
                format!("tool execution cancelled: {action}"),
            );
            Some(TurnOutcome::new(TURN_ESC))
        }
        ToolRunOutcome::Ran => Some(TurnOutcome::new(TURN_TOOL)),
    }
}

/// Handle a plain text answer, an empty-turn nudge, or a forced-final nudge.
fn handle_empty_or_final(
    messages: &mut Vec<Value>,
    tr: &TurnRequest<'_>,
    text: &str,
    ui: &mut dyn UserInterface,
) -> TurnOutcome {
    if !text.trim().is_empty() {
        messages.push(json!({"role": "assistant", "content": text}));
        return TurnOutcome::new(TURN_DONE);
    }
    if !tr.forced_final
        && tr.config.empty_turn_retry_limit > 0
        && tr.empty_turn_count < tr.config.empty_turn_retry_limit
    {
        let dangling = last_is_dangling_tool(messages);
        let tag = if dangling {
            " - dangling tool result"
        } else {
            ""
        };
        ui.message(
            MessageKind::Warning,
            format!(
                "EMPTY TURN{tag}; nudging ({}/{})",
                tr.empty_turn_count + 1,
                tr.config.empty_turn_retry_limit
            ),
        );
        nudge_current_user_turn(messages, EMPTY_TURN_NUDGE);
        return TurnOutcome::new(TURN_EMPTY);
    }
    if !tr.forced_final && tr.config.empty_turn_retry_limit > 0 {
        ui.message(
            MessageKind::Warning,
            "EMPTY TURN - forcing final".to_string(),
        );
        nudge_current_user_turn(messages, FORCED_FINAL_NUDGE);
        return TurnOutcome::new(TURN_FORCE_FINAL);
    }
    if tr.forced_final {
        // The forced final is the last thing the run had to try, so there is no
        // answer to save. Named separately from the case below because the cause
        // is usually a `max_tokens` spent entirely on reasoning, which is
        // something the caller can act on.
        return no_answer(
            "FORCED FINAL RETURNED NO ANSWER - raise AFI_MAX_TOKENS, or lower the effort"
                .to_string(),
            ui,
        );
    }
    // No text, no tool call, and no nudge left to spend: empty-turn retries are
    // exhausted or switched off. Silent until now, which is how a run with
    // nothing to show reported success.
    no_answer(
        "NO ANSWER - the turn produced no text and no tool call".to_string(),
        ui,
    )
}
