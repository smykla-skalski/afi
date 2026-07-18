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

/// Mutable fold state accumulated across the SSE chunks. Splitting the per-kind
/// handling into methods keeps [`accumulate`] itself simple.
struct Fold {
    content_parts: Vec<String>,
    tool_calls: HashMap<u32, ToolCallAccum>,
    reasoning_parts: Vec<String>,
    usage: Option<Usage>,
    timings: Option<Timings>,
    finish_reasons: Vec<String>,
    streamed_chars: u64,
    reasoning_only_chars: usize,
    t_first: Option<f64>,
    mode: TurnMode,
}

impl Fold {
    fn new() -> Self {
        Self {
            content_parts: Vec::new(),
            tool_calls: HashMap::new(),
            reasoning_parts: Vec::new(),
            usage: None,
            timings: None,
            finish_reasons: Vec::new(),
            streamed_chars: 0,
            reasoning_only_chars: 0,
            t_first: None,
            mode: TurnMode::Idle,
        }
    }

    fn no_output_yet(&self) -> bool {
        self.content_parts.is_empty() && self.tool_calls.is_empty()
    }

    /// Capture usage/timings/finish-reason metadata and the time-to-first-token.
    fn note_meta(&mut self, chunk: &StreamChunk, t0: Instant) {
        if chunk.usage.is_some() {
            self.usage.clone_from(&chunk.usage);
        }
        if chunk.timings.is_some() {
            self.timings.clone_from(&chunk.timings);
        }
        if let Some(fr) = &chunk.finish_reason {
            self.finish_reasons.push(fr.clone());
        }
        if self.t_first.is_none()
            && (chunk.content.is_some()
                || !chunk.tool_calls.is_empty()
                || chunk.reasoning_content.is_some())
        {
            self.t_first = Some(t0.elapsed().as_secs_f64());
        }
    }

    /// Echo a reasoning fragment. Returns `true` when the reasoning-only
    /// character limit is reached and the caller should stall.
    fn handle_reasoning(&mut self, rc: &str, config: &ModelConfig) -> bool {
        if !matches!(self.mode, TurnMode::Think) {
            println!("\x1b[2m  -- reasoning --\x1b[0m");
            self.mode = TurnMode::Think;
        }
        if !rc.trim().is_empty() {
            print!("\x1b[2m{rc}\x1b[0m");
            self.reasoning_parts.push(rc.to_string());
        }
        if self.no_output_yet() {
            self.reasoning_only_chars += rc.len();
        }
        config.reasoning_only_char_limit > 0
            && self.no_output_yet()
            && self.reasoning_only_chars >= config.reasoning_only_char_limit
    }

    /// Echo an answer-content fragment.
    fn handle_content(&mut self, c: &str) {
        if matches!(self.mode, TurnMode::Think) {
            println!();
            println!("\x1b[2m  ---------------\x1b[0m");
        }
        if !matches!(self.mode, TurnMode::Say) {
            print!("\x1b[32m");
        }
        self.mode = TurnMode::Say;
        if !c.trim().is_empty() {
            print!("{c}");
            self.content_parts.push(c.to_string());
            self.streamed_chars += c.len() as u64;
        }
    }

    /// Close any open reasoning/answer block before tool-call output.
    fn exit_text_mode(&mut self) {
        if matches!(self.mode, TurnMode::Think) {
            println!();
            println!("\x1b[2m  ---------------\x1b[0m");
            self.mode = TurnMode::Idle;
        } else if matches!(self.mode, TurnMode::Say) {
            println!("\x1b[0m");
            self.mode = TurnMode::Idle;
        }
    }

    /// Fold the tool-call fragments in one chunk into the accumulator.
    fn handle_tool_calls(&mut self, chunk: &StreamChunk) {
        for tc in &chunk.tool_calls {
            self.exit_text_mode();
            let entry = self.tool_calls.entry(tc.index).or_default();
            if tc.id.is_some() {
                entry.id.clone_from(&tc.id);
            }
            if tc.name.is_some() {
                entry.name.clone_from(&tc.name);
            }
            if let Some(args) = &tc.arguments {
                entry.args.push_str(args);
                self.streamed_chars += args.len() as u64;
            }
        }
    }

    /// Close out any open reasoning/answer block on stream end.
    fn finish_line(&mut self) {
        if matches!(self.mode, TurnMode::Think) {
            println!();
            println!("\x1b[2m  ---------------\x1b[0m");
        }
        if matches!(self.mode, TurnMode::Say) {
            println!("\x1b[0m");
        }
        println!("\x1b[0m");
    }

    fn into_accumulated(self) -> Accumulated {
        Accumulated {
            content_parts: self.content_parts,
            tool_calls: self.tool_calls,
            reasoning_parts: self.reasoning_parts,
            usage: self.usage,
            timings: self.timings,
            finish_reasons: self.finish_reasons,
            streamed_chars: self.streamed_chars,
            reasoning_only_chars: self.reasoning_only_chars,
            t_first: self.t_first,
        }
    }
}

/// Fold `chunks` into an [`Accumulated`], echoing reasoning and answer text as
/// it goes. Returns early with [`StreamResult::ReasoningStall`] if the model
/// exceeds the reasoning-only character limit without producing content.
pub(crate) fn accumulate(
    chunks: &[StreamChunk],
    config: &ModelConfig,
    t0: Instant,
) -> StreamResult {
    let mut fold = Fold::new();

    for chunk in chunks {
        fold.note_meta(chunk, t0);

        if let Some(rc) = &chunk.reasoning_content {
            if fold.handle_reasoning(rc, config) {
                println!();
                eprintln!(
                    "\x1b[31m  \u{26a0} REASONING-ONLY LIMIT - {} chars; cutting\x1b[0m",
                    abbr(fold.reasoning_only_chars as u64)
                );
                return StreamResult::ReasoningStall {
                    chars: fold.reasoning_only_chars,
                    reasoning_parts: fold.reasoning_parts,
                };
            }
        }

        if let Some(c) = &chunk.content {
            fold.handle_content(c);
        }

        if !chunk.tool_calls.is_empty() {
            fold.handle_tool_calls(chunk);
        }
    }

    fold.finish_line();
    StreamResult::Done(fold.into_accumulated())
}
