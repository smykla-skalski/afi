//! Post-turn reporting: the statistics footer and the reasoning-only stall
//! handler, split out of `turn.rs` to keep each module within the size cap.

use std::collections::HashMap;

use serde_json::Value;

use crate::metrics::abbr;
use crate::model::recovery::{FORCED_FINAL_NUDGE, nudge_current_user_turn};
use crate::model::turn_dispatch::ToolCallAccum;
use crate::model::{ModelConfig, TURN_FORCE_FINAL, TurnOutcome};
use crate::term::{MessageKind, UserInterface};

/// The stall handler: nudge, force a final, or give up.
///
/// Giving up is a failed turn rather than a finished one. The model spent tokens
/// in its scratchpad and emitted no answer, and reporting that as DONE left a run
/// with nothing to say exiting 0 - or worse, reporting an earlier turn's text as
/// its answer.
pub(crate) fn handle_reasoning_stall(
    messages: &mut Vec<Value>,
    config: &ModelConfig,
    cut_count: u32,
    chars: usize,
    _reasoning_parts: &[String],
    forced_final: bool,
    ui: &mut dyn UserInterface,
) -> TurnOutcome {
    if forced_final {
        return TurnOutcome::no_answer(
            ui,
            format!(
                "FORCED FINAL FAILED - {} reasoning chars",
                abbr(chars as u64)
            ),
        );
    }
    let retry_limit = config.reasoning_only_retry_limit;
    if cut_count >= retry_limit {
        return TurnOutcome::no_answer(
            ui,
            format!("REASONING-ONLY RESCUE FAILED - gave up after {cut_count} stalls"),
        );
    }
    let is_last = cut_count == retry_limit - 1;
    if is_last {
        ui.message(
            MessageKind::Warning,
            format!(
                "REASONING-ONLY STALL - {} chars; forcing final ({}/{})",
                abbr(chars as u64),
                cut_count + 1,
                retry_limit
            ),
        );
        nudge_current_user_turn(messages, FORCED_FINAL_NUDGE);
        return TurnOutcome::new(TURN_FORCE_FINAL);
    }
    ui.message(
        MessageKind::Warning,
        format!(
            "REASONING-ONLY STALL - {} chars; nudging ({}/{})",
            abbr(chars as u64),
            cut_count + 1,
            retry_limit
        ),
    );
    nudge_current_user_turn(messages, "Now act - emit a tool call now.");
    TurnOutcome::new(TURN_FORCE_FINAL)
}

pub(crate) struct TurnStats<'a> {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub elapsed: f64,
    pub t_first: Option<f64>,
    pub streamed_chars: u64,
    pub text: &'a str,
    pub tool_calls: &'a HashMap<u32, ToolCallAccum>,
}

pub(crate) fn print_stats_footer(stats: &TurnStats<'_>, ui: &mut dyn UserInterface) {
    if stats.completion_tokens > 0 && stats.elapsed > 0.0 {
        let tps =
            f64::from(u32::try_from(stats.completion_tokens).unwrap_or(u32::MAX)) / stats.elapsed;
        let mut parts = vec![
            format!("{} tok", stats.completion_tokens),
            format!("{:5.1} tok/s", tps),
            format!("{} ctx", abbr(stats.prompt_tokens)),
        ];
        if let Some(ttft) = stats.t_first {
            parts.push(format!("{:4.0}ms ttft", ttft * 1000.0));
        }
        parts.push(format!("{:4.1}s wall", stats.elapsed));
        ui.message(MessageKind::Stats, format!("└ {}", parts.join(" · ")));
    } else if stats.streamed_chars > 0 {
        let gen_n = (stats.streamed_chars / 4).max(1);
        let tps = if stats.elapsed > 0.0 {
            f64::from(u32::try_from(gen_n).unwrap_or(u32::MAX)) / stats.elapsed
        } else {
            0.0
        };
        ui.message(
            MessageKind::Stats,
            format!(
                "└ ≈{} tok · {tps:5.1} tok/s · {:4.1}s wall",
                abbr(gen_n),
                stats.elapsed
            ),
        );
    } else if !stats.text.is_empty() || !stats.tool_calls.is_empty() {
        ui.message(MessageKind::Stats, format!("└ {:4.1}s wall", stats.elapsed));
    }
}

#[cfg(test)]
mod tests;
