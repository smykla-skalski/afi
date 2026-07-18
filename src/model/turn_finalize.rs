//! Post-stream finalization for a model turn: print the stats footer, then run
//! the reasoning-stall / forced-final / tool-dispatch / empty-turn phases in
//! order and return the first terminal TURN_* status.

use std::collections::HashMap;
use std::time::Instant;

use serde_json::{Value, json};

use crate::model::recovery::{
    EMPTY_TURN_NUDGE, FORCED_FINAL_NUDGE, last_is_dangling_tool, nudge_current_user_turn,
};
use crate::model::stream::normalize_usage;
use crate::model::turn::TurnRequest;
use crate::model::turn_dispatch::{
    DispatchArgs, ToolCallAccum, ToolRunOutcome, dispatch_structured, dispatch_text,
    order_tool_calls,
};
use crate::model::turn_stats::{handle_reasoning_stall, print_stats_footer};
use crate::model::turn_stream::Accumulated;
use crate::model::{TURN_DONE, TURN_EMPTY, TURN_ESC, TURN_FORCE_FINAL, TURN_STREAM_CUT, TURN_TOOL};
use crate::tools::protocol::parse_text_calls;

/// Post-stream: print stats, then run the reasoning-stall / forced-final /
/// tool-dispatch / empty-turn phases in order, returning the first terminal
/// TURN_* status.
pub(crate) fn finalize_turn(
    messages: &mut Vec<Value>,
    tr: &TurnRequest<'_>,
    acc: Accumulated,
    t0: Instant,
) -> String {
    let Accumulated {
        content_parts,
        tool_calls,
        reasoning_parts,
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
        prompt_tokens,
        completion_tokens,
        elapsed,
        t_first,
        streamed_chars,
        &text,
        &tool_calls,
    );
    let _ = normalize_usage(usage.as_ref(), timings.as_ref(), streamed_chars);

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
        );
    }
    if let Some(status) = forced_final_result(messages, tr, &tool_calls, &text, &finish_reasons) {
        return status;
    }
    if let Some(status) = run_structured_tools(messages, tr, &tool_calls, &text) {
        return status;
    }
    if let Some(status) = run_text_tools(messages, tr, &text) {
        return status;
    }
    handle_empty_or_final(messages, tr, &text)
}

/// The forced-final answer / token-limit outcomes, if `forced_final` is set.
fn forced_final_result(
    messages: &mut Vec<Value>,
    tr: &TurnRequest<'_>,
    tool_calls: &HashMap<u32, ToolCallAccum>,
    text: &str,
    finish_reasons: &[String],
) -> Option<String> {
    if tr.forced_final && !tool_calls.is_empty() {
        return Some(emit_forced_final(messages, tool_calls));
    }
    if tr.forced_final
        && !text.trim().is_empty()
        && finish_reasons.iter().any(|f| f.contains("length"))
    {
        eprintln!("\x1b[33m  \u{2702} FORCED FINAL HIT TOKEN LIMIT - saved partial\x1b[0m");
        messages.push(json!({"role": "assistant", "content": format!("{}\n\n[Truncated by token limit before completion.]", text.trim_end())}));
        return Some(TURN_DONE.to_string());
    }
    None
}

/// Extract the `final_answer` tool call (or report the miss). Always DONE.
fn emit_forced_final(
    messages: &mut Vec<Value>,
    tool_calls: &HashMap<u32, ToolCallAccum>,
) -> String {
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
            eprintln!("\x1b[31m  \u{2702} FORCED FINAL ANSWER EMPTY\x1b[0m");
        } else {
            println!("\x1b[32m{answer}\x1b[0m");
            messages.push(json!({"role": "assistant", "content": answer}));
        }
        return TURN_DONE.to_string();
    }
    let names: Vec<&str> = ordered
        .iter()
        .map(|c| c.name.as_deref().unwrap_or("tool"))
        .collect();
    eprintln!(
        "\x1b[31m  \u{2702} FORCED FINAL FAILED - model emitted {}\x1b[0m",
        names.join(", ")
    );
    TURN_DONE.to_string()
}

/// The `DispatchArgs` view of a turn request.
fn dispatch_args<'a>(tr: &TurnRequest<'a>) -> DispatchArgs<'a> {
    DispatchArgs {
        approval: tr.approval,
        classifier: tr.classifier,
        cwd: tr.cwd,
        project_root: tr.project_root,
        env: tr.env,
        config: tr.config,
    }
}

/// Push the assistant turn that carries the structured tool calls.
fn push_assistant_tool_calls(messages: &mut Vec<Value>, ordered: &[ToolCallAccum], text: &str) {
    let tool_calls_json: Vec<Value> = ordered.iter().map(|c| json!({"id": c.id.clone().unwrap_or_default(), "type": "function", "function": {"name": c.name.clone().unwrap_or_default(), "arguments": c.args.clone()}})).collect();
    let content = if text.trim().is_empty() {
        Value::Null
    } else {
        json!(text)
    };
    messages.push(json!({"role": "assistant", "content": content, "tool_calls": tool_calls_json}));
}

/// Warn about (and either give up or retry) a malformed tool-call payload.
fn malformed_tool_retry(
    messages: &mut Vec<Value>,
    tr: &TurnRequest<'_>,
    ordered: &[ToolCallAccum],
    idx: usize,
    err: &str,
) -> String {
    let name = ordered[idx].name.as_deref().unwrap_or("tool");
    let retry_limit = tr.config.malformed_stream_retry_limit;
    if tr.malformed_stream_cut_count >= retry_limit {
        eprintln!(
            "\x1b[31m  \u{2717} malformed tool call after {} recoveries\x1b[0m",
            tr.malformed_stream_cut_count
        );
        return TURN_DONE.to_string();
    }
    eprintln!(
        "\x1b[33m  \u{2702} MALFORMED TOOL CALL - {} args invalid ({}); retrying ({}/{})\x1b[0m",
        name,
        err,
        tr.malformed_stream_cut_count + 1,
        retry_limit
    );
    nudge_current_user_turn(
        messages,
        "Your previous tool call had malformed JSON arguments. Retry with valid arguments.",
    );
    TURN_STREAM_CUT.to_string()
}

/// Dispatch structured (`tool_calls`) calls, if any. `None` when there are none.
fn run_structured_tools(
    messages: &mut Vec<Value>,
    tr: &TurnRequest<'_>,
    tool_calls: &HashMap<u32, ToolCallAccum>,
    text: &str,
) -> Option<String> {
    if tool_calls.is_empty() {
        return None;
    }
    let ordered = order_tool_calls(tool_calls);
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
                ));
            }
        }
    }
    push_assistant_tool_calls(messages, &ordered, text);
    let da = dispatch_args(tr);
    match dispatch_structured(
        messages,
        &ordered,
        &parsed_args,
        &da,
        tr.config.tool_result_chars,
    ) {
        ToolRunOutcome::Escaped(action) => {
            eprintln!("\x1b[33m  \u{21b3} escaped approval of {action:?}\x1b[0m");
            messages.push(json!({"role": "user", "content": "[User pressed Esc at a tool approval prompt. Acknowledge briefly and wait.]"}));
            Some(TURN_ESC.to_string())
        }
        ToolRunOutcome::Ran => Some(TURN_TOOL.to_string()),
    }
}

/// Dispatch text-protocol tool calls, if any. `None` when there are none.
fn run_text_tools(messages: &mut Vec<Value>, tr: &TurnRequest<'_>, text: &str) -> Option<String> {
    let calls = parse_text_calls(text);
    if calls.is_empty() {
        return None;
    }
    messages.push(json!({"role": "assistant", "content": text}));
    let da = dispatch_args(tr);
    match dispatch_text(messages, &calls, &da) {
        ToolRunOutcome::Escaped(action) => {
            eprintln!("\x1b[33m  \u{21b3} escaped approval of {action:?}\x1b[0m");
            Some(TURN_ESC.to_string())
        }
        ToolRunOutcome::Ran => Some(TURN_TOOL.to_string()),
    }
}

/// Handle a plain text answer, an empty-turn nudge, or a forced-final nudge.
fn handle_empty_or_final(messages: &mut Vec<Value>, tr: &TurnRequest<'_>, text: &str) -> String {
    if !text.trim().is_empty() {
        messages.push(json!({"role": "assistant", "content": text}));
        return TURN_DONE.to_string();
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
        eprintln!(
            "\x1b[33m  \u{2702} EMPTY TURN{}; nudging ({}/{})\x1b[0m",
            tag,
            tr.empty_turn_count + 1,
            tr.config.empty_turn_retry_limit
        );
        nudge_current_user_turn(messages, EMPTY_TURN_NUDGE);
        return TURN_EMPTY.to_string();
    }
    if !tr.forced_final && tr.config.empty_turn_retry_limit > 0 {
        eprintln!("\x1b[33m  \u{2702} EMPTY TURN - forcing final\x1b[0m");
        nudge_current_user_turn(messages, FORCED_FINAL_NUDGE);
        return TURN_FORCE_FINAL.to_string();
    }
    TURN_DONE.to_string()
}
