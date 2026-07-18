//! The model turn: one streamed turn (open stream, process SSE chunks,
//! dispatch tools, return a TURN_* status) and the retry loop that wraps it.

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use serde_json::{json, Value};

use crate::approval::ApprovalState;
use crate::config::Source;
use crate::log::log_event;
use crate::metrics::abbr;
use crate::model::client::{ChatClient, ClientError, StreamRequest};
use crate::model::recovery::{
    last_is_dangling_tool, nudge_current_user_turn, recovery_sampling_opts, EMPTY_TURN_NUDGE,
    FORCED_FINAL_NUDGE,
};
use crate::model::stream::normalize_usage;
use crate::model::{
    ModelConfig, FINAL_ANSWER_TOOL, FINAL_ANSWER_TOOL_CHOICE, TURN_DONE, TURN_EMPTY, TURN_ESC,
    TURN_FORCE_FINAL, TURN_STREAM_CUT, TURN_TOOL,
};
use crate::risk::{confirm, ApprovalChoice, RiskClassifier};
use crate::tools;
use crate::tools::protocol::{parse_text_calls, sanitize_tool_result};

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

    let chunks = match tr.client.chat_completions_stream(stream_req).await {
        Ok(c) => c,
        Err(ClientError::Connection(msg)) => {
            eprintln!(
                "\x1b[31m  \u{2717} can't reach {} - is the server up?\n    {}\x1b[0m",
                tr.source.base_url, msg
            );
            return TURN_DONE.to_string();
        }
        Err(ClientError::Http { status, body }) => {
            let body_short = &body[..body.len().min(200)];
            eprintln!("\x1b[31m  \u{2717} HTTP {}: {}\x1b[0m", status, body_short);
            return TURN_DONE.to_string();
        }
        Err(ClientError::Parse(msg)) => {
            eprintln!("\x1b[31m  \u{2717} parse error: {}\x1b[0m", msg);
            return TURN_DONE.to_string();
        }
    };

    log_event("resp", &json!({"chunks": chunks.len()}));

    let mut content_parts: Vec<String> = Vec::new();
    let mut tool_calls: HashMap<u32, ToolCallAccum> = HashMap::new();
    let mut reasoning_parts: Vec<String> = Vec::new();
    let mut usage = None;
    let mut timings = None;
    let mut finish_reasons: Vec<String> = Vec::new();
    let mut streamed_chars: u64 = 0;
    let mut reasoning_only_chars: usize = 0;
    let mut t_first: Option<f64> = None;
    let mut mode = TurnMode::Idle;

    for chunk in &chunks {
        if chunk.usage.is_some() {
            usage = chunk.usage.clone();
        }
        if chunk.timings.is_some() {
            timings = chunk.timings.clone();
        }
        if chunk.finish_reason.is_some() {
            finish_reasons.push(chunk.finish_reason.clone().unwrap_or_default());
        }
        if t_first.is_none()
            && (chunk.content.is_some()
                || !chunk.tool_calls.is_empty()
                || chunk.reasoning_content.is_some())
        {
            t_first = Some(t0.elapsed().as_secs_f64());
        }

        if let Some(rc) = &chunk.reasoning_content {
            if !matches!(mode, TurnMode::Think) {
                println!("\x1b[2m  -- reasoning --\x1b[0m");
                mode = TurnMode::Think;
            }
            if !rc.trim().is_empty() {
                print!("\x1b[2m{}\x1b[0m", rc);
                reasoning_parts.push(rc.clone());
            }
            if content_parts.is_empty() && tool_calls.is_empty() {
                reasoning_only_chars += rc.len();
            }
            if tr.config.reasoning_only_char_limit > 0
                && content_parts.is_empty()
                && tool_calls.is_empty()
                && reasoning_only_chars >= tr.config.reasoning_only_char_limit
            {
                println!();
                eprintln!(
                    "\x1b[31m  \u{26a0} REASONING-ONLY LIMIT - {} chars; cutting\x1b[0m",
                    abbr(reasoning_only_chars as u64)
                );
                return handle_reasoning_stall(
                    messages,
                    tr.config,
                    tr.reasoning_loop_cut_count,
                    reasoning_only_chars,
                    &reasoning_parts,
                    tr.forced_final,
                );
            }
        }

        if let Some(c) = &chunk.content {
            if matches!(mode, TurnMode::Think) {
                println!();
                println!("\x1b[2m  ---------------\x1b[0m");
            }
            if !matches!(mode, TurnMode::Say) {
                print!("\x1b[32m");
            }
            mode = TurnMode::Say;
            if !c.trim().is_empty() {
                print!("{}", c);
                content_parts.push(c.clone());
                streamed_chars += c.len() as u64;
            }
        }

        for tc in &chunk.tool_calls {
            if matches!(mode, TurnMode::Think) {
                println!();
                println!("\x1b[2m  ---------------\x1b[0m");
                mode = TurnMode::Idle;
            } else if matches!(mode, TurnMode::Say) {
                println!("\x1b[0m");
                mode = TurnMode::Idle;
            }
            let entry = tool_calls.entry(tc.index).or_default();
            if tc.id.is_some() {
                entry.id = tc.id.clone();
            }
            if tc.name.is_some() {
                entry.name = tc.name.clone();
            }
            if let Some(args) = &tc.arguments {
                entry.args.push_str(args);
                streamed_chars += args.len() as u64;
            }
        }
    }

    if matches!(mode, TurnMode::Think) {
        println!();
        println!("\x1b[2m  ---------------\x1b[0m");
    }
    if matches!(mode, TurnMode::Say) {
        println!("\x1b[0m");
    }
    println!("\x1b[0m");

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

        let mut esc_action: Option<String> = None;
        for (idx, (c, args)) in ordered.iter().zip(parsed_args.iter()).enumerate() {
            let name = c.name.as_deref().unwrap_or("tool");
            let da = DispatchArgs {
                approval: tr.approval,
                classifier: tr.classifier,
                cwd: tr.cwd,
                project_root: tr.project_root,
                env: tr.env,
                config: tr.config,
            };
            match dispatch_tool(name, args, &da) {
                ToolDispatchResult::Ok(result) => {
                    let sanitized = sanitize_tool_result(&result, tr.config.tool_result_chars);
                    messages.push(json!({"role": "tool", "tool_call_id": c.id.clone().unwrap_or_default(), "content": sanitized}));
                }
                ToolDispatchResult::Escaped(action) => {
                    esc_action = Some(action);
                    messages.push(json!({"role": "tool", "tool_call_id": c.id.clone().unwrap_or_default(), "content": "CANCELLED by user (Esc)"}));
                    for c2 in &ordered[idx + 1..] {
                        messages.push(json!({"role": "tool", "tool_call_id": c2.id.clone().unwrap_or_default(), "content": "SKIPPED"}));
                    }
                    break;
                }
            }
        }
        if let Some(action) = esc_action {
            eprintln!("\x1b[33m  \u{21b3} escaped approval of {:?}\x1b[0m", action);
            messages.push(json!({"role": "user", "content": "[User pressed Esc at a tool approval prompt. Acknowledge briefly and wait.]"}));
            return TURN_ESC.to_string();
        }
        return TURN_TOOL.to_string();
    }

    let calls = parse_text_calls(&text);
    if !calls.is_empty() {
        messages.push(json!({"role": "assistant", "content": text}));
        let mut observations: Vec<String> = Vec::new();
        let mut esc_action: Option<String> = None;
        for (name, args) in &calls {
            let da2 = DispatchArgs {
                approval: tr.approval,
                classifier: tr.classifier,
                cwd: tr.cwd,
                project_root: tr.project_root,
                env: tr.env,
                config: tr.config,
            };
            match dispatch_tool(name, args, &da2) {
                ToolDispatchResult::Ok(r) => {
                    observations.push(format!("Observation ({}): {}", name, r));
                }
                ToolDispatchResult::Escaped(action) => {
                    esc_action = Some(action);
                    observations.push(format!("Observation ({}): CANCELLED", name));
                    break;
                }
            }
        }
        if let Some(action) = esc_action {
            eprintln!("\x1b[33m  \u{21b3} escaped approval of {:?}\x1b[0m", action);
            messages.push(json!({"role": "user", "content": observations.join("\n")}));
            return TURN_ESC.to_string();
        }
        messages.push(json!({"role": "user", "content": observations.join("\n")}));
        return TURN_TOOL.to_string();
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

#[derive(Default)]
struct ToolCallAccum {
    id: Option<String>,
    name: Option<String>,
    args: String,
}

fn order_tool_calls(tcs: &HashMap<u32, ToolCallAccum>) -> Vec<ToolCallAccum> {
    let mut indices: Vec<u32> = tcs.keys().copied().collect();
    indices.sort();
    indices
        .into_iter()
        .filter_map(|i| {
            tcs.get(&i).map(|c| ToolCallAccum {
                id: c.id.clone(),
                name: c.name.clone(),
                args: c.args.clone(),
            })
        })
        .collect()
}

#[derive(PartialEq)]
enum TurnMode {
    Idle,
    Think,
    Say,
}

enum ToolDispatchResult {
    Ok(String),
    Escaped(String),
}

/// Bundles the parameters for dispatching a tool call.
struct DispatchArgs<'a> {
    approval: &'a ApprovalState,
    classifier: &'a dyn RiskClassifier,
    cwd: &'a Path,
    project_root: &'a Path,
    env: &'a HashMap<String, String>,
    config: &'a ModelConfig,
}

fn dispatch_tool(name: &str, args: &Value, da: &DispatchArgs<'_>) -> ToolDispatchResult {
    let action = match name {
        "write_file" => format!(
            "write {} ({} bytes)",
            args.get("path").and_then(|p| p.as_str()).unwrap_or("?"),
            args.get("content")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .len()
        ),
        "edit_file" => format!(
            "edit {}",
            args.get("path").and_then(|p| p.as_str()).unwrap_or("?")
        ),
        "run_bash" => format!(
            "run: {}",
            args.get("command").and_then(|c| c.as_str()).unwrap_or("?")
        ),
        _ => name.to_string(),
    };

    if matches!(name, "write_file" | "edit_file" | "run_bash") {
        let ask: fn(&str) -> ApprovalChoice = |_| ApprovalChoice::Yes;
        match confirm(
            &action,
            da.approval,
            da.classifier,
            da.cwd,
            da.project_root,
            &ask,
        ) {
            Ok(true) => {}
            Ok(false) => return ToolDispatchResult::Ok("DENIED by user".to_string()),
            Err(e) => return ToolDispatchResult::Escaped(e.0),
        }
    }

    let result = match name {
        "read_file" => tools::read_file(
            args.get("path").and_then(|p| p.as_str()).unwrap_or(""),
            args.get("offset").and_then(|o| o.as_i64()),
            args.get("limit").and_then(|l| l.as_i64()),
            da.config.read_file_lines,
        ),
        "write_file" => tools::write_file(
            args.get("path").and_then(|p| p.as_str()).unwrap_or(""),
            args.get("content").and_then(|c| c.as_str()).unwrap_or(""),
        ),
        "edit_file" => tools::edit_file(
            args.get("path").and_then(|p| p.as_str()).unwrap_or(""),
            args.get("old").and_then(|o| o.as_str()).unwrap_or(""),
            args.get("new").and_then(|n| n.as_str()).unwrap_or(""),
        ),
        "list_dir" => tools::list_dir(args.get("path").and_then(|p| p.as_str()).unwrap_or(".")),
        "run_bash" => {
            let command = args.get("command").and_then(|c| c.as_str()).unwrap_or("");
            if command.is_empty() {
                return ToolDispatchResult::Ok("ERROR: run_bash requires 'command'.".to_string());
            }
            tools::bash::run_bash(
                command,
                args.get("timeout").and_then(|t| t.as_i64()),
                da.env,
                &|| false,
            )
        }
        "wait_background" => {
            let pid = args.get("pid").and_then(|p| p.as_u64()).unwrap_or(0) as u32;
            let timeout = args.get("timeout").and_then(|t| t.as_i64()).unwrap_or(0);
            let log_path = args
                .get("log_path")
                .and_then(|l| l.as_str())
                .map(std::path::PathBuf::from);
            tools::bash::wait_background(pid, log_path.as_deref(), timeout, da.env, &|| false)
        }
        "final_answer" => {
            return ToolDispatchResult::Ok(
                args.get("answer")
                    .and_then(|a| a.as_str())
                    .unwrap_or("")
                    .to_string(),
            )
        }
        _ => format!("ERROR: unknown tool {}", name),
    };
    ToolDispatchResult::Ok(result)
}

fn handle_reasoning_stall(
    messages: &mut Vec<Value>,
    config: &ModelConfig,
    cut_count: u32,
    chars: usize,
    _reasoning_parts: &[String],
    forced_final: bool,
) -> String {
    if forced_final {
        eprintln!(
            "\x1b[31m  \u{2702} FORCED FINAL FAILED - {} reasoning chars\x1b[0m",
            abbr(chars as u64)
        );
        return TURN_DONE.to_string();
    }
    let retry_limit = config.reasoning_only_retry_limit;
    if cut_count >= retry_limit {
        eprintln!(
            "\x1b[31m  \u{2702} REASONING-ONLY RESCUE FAILED - gave up after {} stalls\x1b[0m",
            cut_count
        );
        return TURN_DONE.to_string();
    }
    let is_last = cut_count == retry_limit - 1;
    if is_last {
        eprintln!(
            "\x1b[33m  \u{2702} REASONING-ONLY STALL - {} chars; forcing final ({}/{})\x1b[0m",
            abbr(chars as u64),
            cut_count + 1,
            retry_limit
        );
        nudge_current_user_turn(messages, FORCED_FINAL_NUDGE);
        return TURN_FORCE_FINAL.to_string();
    }
    eprintln!(
        "\x1b[33m  \u{2702} REASONING-ONLY STALL - {} chars; nudging ({}/{})\x1b[0m",
        abbr(chars as u64),
        cut_count + 1,
        retry_limit
    );
    nudge_current_user_turn(messages, "Now act - emit a tool call now.");
    TURN_FORCE_FINAL.to_string()
}

fn print_stats_footer(
    prompt_tokens: u64,
    completion_tokens: u64,
    elapsed: f64,
    t_first: Option<f64>,
    streamed_chars: u64,
    text: &str,
    tool_calls: &HashMap<u32, ToolCallAccum>,
) {
    let dim = "\x1b[2m";
    let reset = "\x1b[0m";
    if completion_tokens > 0 && elapsed > 0.0 {
        let tps = completion_tokens as f64 / elapsed;
        let mut parts = vec![
            format!("{} tok", completion_tokens),
            format!("{:5.1} tok/s", tps),
            format!("{} ctx", abbr(prompt_tokens)),
        ];
        if let Some(ttft) = t_first {
            parts.push(format!("{:4.0}ms ttft", ttft * 1000.0));
        }
        parts.push(format!("{:4.1}s wall", elapsed));
        println!("{}  \u{2514} {}{}", dim, parts.join(" \u{00b7} "), reset);
    } else if streamed_chars > 0 {
        let gen_n = (streamed_chars / 4).max(1);
        let tps = if elapsed > 0.0 {
            gen_n as f64 / elapsed
        } else {
            0.0
        };
        println!(
            "{}  \u{2514} \u{2248}{} tok \u{00b7} {:5.1} tok/s \u{00b7} {:4.1}s wall{}",
            dim,
            abbr(gen_n),
            tps,
            elapsed,
            reset
        );
    } else if !text.is_empty() || !tool_calls.is_empty() {
        println!("{}  \u{2514} {:4.1}s wall{}", dim, elapsed, reset);
    }
}

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
        .get("MINION_MAX_MODEL_TURNS")
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
