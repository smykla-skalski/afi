//! Streaming accumulation for a model turn: fold the SSE chunks into content,
//! reasoning, and tool-call fragments while echoing reasoning/answer text, and
//! flag a reasoning-only stall when the model loops without acting.

use std::collections::HashMap;
use std::time::Instant;

use crate::metrics::abbr;
use crate::model::stream::{StreamChunk, Timings, Usage};
use crate::model::turn_dispatch::ToolCallAccum;
use crate::model::ModelConfig;

#[derive(PartialEq)]
enum TurnMode {
    Idle,
    Think,
    Say,
}

/// The folded result of a streamed turn.
pub(crate) struct Accumulated {
    pub content_parts: Vec<String>,
    pub tool_calls: HashMap<u32, ToolCallAccum>,
    pub reasoning_parts: Vec<String>,
    pub usage: Option<Usage>,
    pub timings: Option<Timings>,
    pub finish_reasons: Vec<String>,
    pub streamed_chars: u64,
    pub reasoning_only_chars: usize,
    pub t_first: Option<f64>,
}

/// Either the folded turn, or a reasoning-only stall the caller must handle.
pub(crate) enum StreamResult {
    Done(Accumulated),
    ReasoningStall {
        chars: usize,
        reasoning_parts: Vec<String>,
    },
}

/// Fold `chunks` into an [`Accumulated`], echoing reasoning and answer text as
/// it goes. Returns early with [`StreamResult::ReasoningStall`] if the model
/// exceeds the reasoning-only character limit without producing content.
pub(crate) fn accumulate(
    chunks: &[StreamChunk],
    config: &ModelConfig,
    t0: Instant,
) -> StreamResult {
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

    for chunk in chunks {
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
            if config.reasoning_only_char_limit > 0
                && content_parts.is_empty()
                && tool_calls.is_empty()
                && reasoning_only_chars >= config.reasoning_only_char_limit
            {
                println!();
                eprintln!(
                    "\x1b[31m  \u{26a0} REASONING-ONLY LIMIT - {} chars; cutting\x1b[0m",
                    abbr(reasoning_only_chars as u64)
                );
                return StreamResult::ReasoningStall {
                    chars: reasoning_only_chars,
                    reasoning_parts,
                };
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

    StreamResult::Done(Accumulated {
        content_parts,
        tool_calls,
        reasoning_parts,
        usage,
        timings,
        finish_reasons,
        streamed_chars,
        reasoning_only_chars,
        t_first,
    })
}
