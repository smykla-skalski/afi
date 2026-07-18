//! Post-turn reporting: the statistics footer and the reasoning-only stall
//! handler, split out of `turn.rs` to keep each module within the size cap.

use std::collections::HashMap;

use serde_json::Value;

use crate::metrics::abbr;
use crate::model::recovery::{nudge_current_user_turn, FORCED_FINAL_NUDGE};
use crate::model::turn_dispatch::ToolCallAccum;
use crate::model::{ModelConfig, TURN_DONE, TURN_FORCE_FINAL};

pub(crate) fn handle_reasoning_stall(
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

pub(crate) fn print_stats_footer(
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
