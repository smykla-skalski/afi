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

use serde_json::{json, Value};

use crate::approval::ApprovalState;
use crate::config::Source;
use crate::log::log_event;
use crate::model::client::{ChatClient, ClientError, StreamRequest};
use crate::model::recovery::{
    last_is_dangling_tool, nudge_current_user_turn, recovery_sampling_opts, EMPTY_TURN_NUDGE,
    FORCED_FINAL_NUDGE,
};
use crate::model::stream::normalize_usage;
use crate::model::turn_dispatch::{
    dispatch_structured, dispatch_text, order_tool_calls, DispatchArgs, ToolRunOutcome,
};
use crate::model::turn_stats::{handle_reasoning_stall, print_stats_footer};
use crate::model::turn_stream::{accumulate, Accumulated, StreamResult};
use crate::model::{
    ModelConfig, FINAL_ANSWER_TOOL, FINAL_ANSWER_TOOL_CHOICE, TURN_DONE, TURN_EMPTY, TURN_ESC,
    TURN_FORCE_FINAL, TURN_STREAM_CUT, TURN_TOOL,
};
use crate::risk::RiskClassifier;
use crate::term::activity::{run_during_generation, Generation};
use crate::term::interrupt::InterruptWatcher;
use crate::tools;
use crate::tools::protocol::parse_text_calls;

pub use crate::model::turn_loop::{run_model_turn_loop, LoopRequest};

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

    let tools_val = if tr.forced_final {
        json!([FINAL_ANSWER_TOOL.clone()])
    } else {
        tools::TOOLS.clone()
    };
    let tool_choice = if tr.forced_final {
        Some(FINAL_ANSWER_TOOL_CHOICE.clone())
    } else {
        None
    };
    let max_tokens = if tr.forced_final {
        Some(tr.config.forced_final_max_tokens)
    } else if tr.config.max_completion_tokens > 0 {
        Some(tr.config.max_completion_tokens)
    } else {
        None
    };

    let mut extra_body = tr.source.extra_body.clone();
    if tr.recovery_sampling {
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
    }

    let stream_req = StreamRequest {
        source: tr.source,
        model: tr.model,
        messages: messages.as_slice(),
        tools: Some(&tools_val),
        tool_choice: tool_choice.as_ref(),
        max_tokens,
        extra_body: extra_body.as_ref(),
        recovery_sampling: tr.recovery_sampling,
    };

    log_event(
        "req",
        &json!({"model": tr.model, "stream": true, "forced_final": tr.forced_final, "recovery_sampling": tr.recovery_sampling}),
    );

    let interrupt = InterruptWatcher::new();
    let stream_fut = tr.client.chat_completions_stream(stream_req);
    let chunks = match run_during_generation(&interrupt, "thinking", stream_fut).await {
        Generation::Completed(Ok(c)) => c,
        Generation::Completed(Err(ClientError::Connection(msg))) => {
            eprintln!(
                "\x1b[31m  \u{2717} can't reach {} - is the server up?\n    {}\x1b[0m",
                tr.source.base_url, msg
            );
            return TURN_DONE.to_string();
        }
        Generation::Completed(Err(ClientError::Http { status, body })) => {
            let body_short = &body[..body.len().min(200)];
            eprintln!("\x1b[31m  \u{2717} HTTP {}: {}\x1b[0m", status, body_short);
            return TURN_DONE.to_string();
        }
        Generation::Completed(Err(ClientError::Parse(msg))) => {
            eprintln!("\x1b[31m  \u{2717} parse error: {}\x1b[0m", msg);
            return TURN_DONE.to_string();
        }
        Generation::Interrupted => {
            eprintln!("\x1b[33m  \u{21b3} interrupted by Esc\x1b[0m");
            messages.push(json!({"role": "user", "content": "[User pressed Esc to interrupt generation. Acknowledge briefly and wait.]"}));
            return TURN_ESC.to_string();
        }
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

    if tr.forced_final && !tool_calls.is_empty() {
        let ordered = order_tool_calls(&tool_calls);
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
            if !answer.is_empty() {
                println!("\x1b[32m{}\x1b[0m", answer);
                messages.push(json!({"role": "assistant", "content": answer}));
                return TURN_DONE.to_string();
            }
            eprintln!("\x1b[31m  \u{2702} FORCED FINAL ANSWER EMPTY\x1b[0m");
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
        return TURN_DONE.to_string();
    }

    if tr.forced_final
        && !text.trim().is_empty()
        && finish_reasons.iter().any(|f| f.contains("length"))
    {
        eprintln!("\x1b[33m  \u{2702} FORCED FINAL HIT TOKEN LIMIT - saved partial\x1b[0m");
        messages.push(json!({"role": "assistant", "content": format!("{}\n\n[Truncated by token limit before completion.]", text.trim_end())}));
        return TURN_DONE.to_string();
    }

    if !tool_calls.is_empty() {
        let ordered = order_tool_calls(&tool_calls);
        let mut parsed_args: Vec<Value> = Vec::new();
        let mut parse_error: Option<(usize, String)> = None;
        for (i, c) in ordered.iter().enumerate() {
            match serde_json::from_str::<Value>(&c.args) {
                Ok(v) => parsed_args.push(v),
                Err(e) => {
                    parse_error = Some((i, e.to_string()));
                    break;
                }
            }
        }
        if let Some((idx, err)) = parse_error {
            let name = ordered[idx].name.as_deref().unwrap_or("tool");
            let retry_limit = tr.config.malformed_stream_retry_limit;
            if tr.malformed_stream_cut_count >= retry_limit {
                eprintln!(
                    "\x1b[31m  \u{2717} malformed tool call after {} recoveries\x1b[0m",
                    tr.malformed_stream_cut_count
                );
                return TURN_DONE.to_string();
            }
            eprintln!("\x1b[33m  \u{2702} MALFORMED TOOL CALL - {} args invalid ({}); retrying ({}/{})\x1b[0m", name, err, tr.malformed_stream_cut_count + 1, retry_limit);
            nudge_current_user_turn(
                messages,
                "Your previous tool call had malformed JSON arguments. Retry with valid arguments.",
            );
            return TURN_STREAM_CUT.to_string();
        }

        let tool_calls_json: Vec<Value> = ordered.iter().map(|c| json!({"id": c.id.clone().unwrap_or_default(), "type": "function", "function": {"name": c.name.clone().unwrap_or_default(), "arguments": c.args.clone()}})).collect();
        messages.push(json!({"role": "assistant", "content": if text.trim().is_empty() { Value::Null } else { json!(text) }, "tool_calls": tool_calls_json}));

        let da = DispatchArgs {
            approval: tr.approval,
            classifier: tr.classifier,
            cwd: tr.cwd,
            project_root: tr.project_root,
            env: tr.env,
            config: tr.config,
        };
        match dispatch_structured(
            messages,
            &ordered,
            &parsed_args,
            &da,
            tr.config.tool_result_chars,
        ) {
            ToolRunOutcome::Escaped(action) => {
                eprintln!("\x1b[33m  \u{21b3} escaped approval of {:?}\x1b[0m", action);
                messages.push(json!({"role": "user", "content": "[User pressed Esc at a tool approval prompt. Acknowledge briefly and wait.]"}));
                return TURN_ESC.to_string();
            }
            ToolRunOutcome::Ran => return TURN_TOOL.to_string(),
        }
    }

    let calls = parse_text_calls(&text);
    if !calls.is_empty() {
        messages.push(json!({"role": "assistant", "content": text}));
        let da = DispatchArgs {
            approval: tr.approval,
            classifier: tr.classifier,
            cwd: tr.cwd,
            project_root: tr.project_root,
            env: tr.env,
            config: tr.config,
        };
        match dispatch_text(messages, &calls, &da) {
            ToolRunOutcome::Escaped(action) => {
                eprintln!("\x1b[33m  \u{21b3} escaped approval of {:?}\x1b[0m", action);
                return TURN_ESC.to_string();
            }
            ToolRunOutcome::Ran => return TURN_TOOL.to_string(),
        }
    }

    if text.trim().is_empty() {
        if !tr.forced_final
            && tr.config.empty_turn_retry_limit > 0
            && tr.empty_turn_count < tr.config.empty_turn_retry_limit
        {
            let dangling = last_is_dangling_tool(messages);
            eprintln!(
                "\x1b[33m  \u{2702} EMPTY TURN{}; nudging ({}/{})\x1b[0m",
                if dangling {
                    " - dangling tool result"
                } else {
                    ""
                },
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
        return TURN_DONE.to_string();
    }

    messages.push(json!({"role": "assistant", "content": text}));
    TURN_DONE.to_string()
}
