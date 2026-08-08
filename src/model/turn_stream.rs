//! Incremental accumulation for a model turn. Each SSE chunk updates the fold
//! and emits typed UI deltas as soon as it arrives.

use std::collections::{BTreeMap, HashMap};
use std::mem::take;
use std::time::Instant;

use serde_json::{Value, json};

use crate::metrics::abbr;
use crate::model::stream::tags::ReasoningTags;
use crate::model::stream::{StreamChunk, ThinkingDelta, Timings, Usage};
use crate::model::turn_dispatch::ToolCallAccum;
use crate::term::{MessageKind, StreamKind, UserInterface};

/// The folded result of a streamed turn.
pub(crate) struct Accumulated {
    pub content_parts: Vec<String>,
    pub tool_calls: HashMap<u32, ToolCallAccum>,
    pub reasoning_parts: Vec<String>,
    /// Anthropic thinking blocks, in the order the model emitted them, ready to
    /// be replayed verbatim on the request carrying this turn's tool results.
    /// Always empty on the `OpenAI` path.
    pub thinking_blocks: Vec<Value>,
    pub usage: Option<Usage>,
    pub timings: Option<Timings>,
    pub finish_reasons: Vec<String>,
    pub streamed_chars: u64,
    pub reasoning_only_chars: usize,
    pub t_first: Option<f64>,
}

/// Either the folded turn, or a reasoning-only stall the caller must handle.
///
/// `Accumulated` is boxed so the stall variant does not carry its footprint.
/// One allocation per turn, against an HTTP round trip.
pub(crate) enum StreamResult {
    Done(Box<Accumulated>),
    ReasoningStall {
        chars: usize,
        reasoning_parts: Vec<String>,
    },
}

/// One thinking block, assembled from its deltas.
#[derive(Default)]
struct ThinkingAccum {
    thinking: String,
    signature: String,
    redacted: Option<String>,
}

impl ThinkingAccum {
    fn apply(&mut self, delta: &ThinkingDelta) {
        if let Some(text) = &delta.thinking {
            self.thinking.push_str(text);
        }
        if let Some(signature) = &delta.signature {
            self.signature.push_str(signature);
        }
        if delta.redacted.is_some() {
            self.redacted.clone_from(&delta.redacted);
        }
    }

    /// The replayable block, or `None` when the stream ended before it was
    /// whole. An unsigned thinking block fails the entire next request, so it
    /// is dropped rather than sent.
    fn into_block(self) -> Option<Value> {
        if let Some(data) = self.redacted {
            return Some(json!({"type": "redacted_thinking", "data": data}));
        }
        if self.signature.is_empty() {
            return None;
        }
        // The text is deliberately not trimmed or normalized: the signature is
        // over these exact bytes, and `display: "omitted"` legitimately leaves
        // it empty.
        Some(json!({
            "type": "thinking",
            "thinking": self.thinking,
            "signature": self.signature,
        }))
    }
}

/// Mutable fold state updated once per live SSE chunk.
pub(crate) struct StreamAccumulator {
    content_parts: Vec<String>,
    tool_calls: HashMap<u32, ToolCallAccum>,
    reasoning_parts: Vec<String>,
    /// Keyed by Anthropic's content-block index, so iteration restores the
    /// order the model emitted the blocks in.
    thinking: BTreeMap<u32, ThinkingAccum>,
    usage: Option<Usage>,
    timings: Option<Timings>,
    finish_reasons: Vec<String>,
    streamed_chars: u64,
    reasoning_only_chars: usize,
    /// The reasoning-only cutoff for this turn. `0` disables the cut, which is
    /// what the Anthropic path uses while thinking is on.
    reasoning_only_char_limit: usize,
    /// Lifts `<think>` and `<reasoning>` spans back out of `content`, for the
    /// endpoints that put deliberation there instead of in its own field. Idle
    /// on the sources that cannot need it.
    tags: ReasoningTags,
    t_first: Option<f64>,
}

impl StreamAccumulator {
    /// `split_reasoning_tags` is off for sources that report deliberation
    /// structurally, where looking for tags could only take a quoted one out of
    /// an answer that meant it.
    #[must_use]
    pub(crate) fn new(reasoning_only_char_limit: usize, split_reasoning_tags: bool) -> Self {
        Self {
            content_parts: Vec::new(),
            tool_calls: HashMap::new(),
            reasoning_parts: Vec::new(),
            thinking: BTreeMap::new(),
            usage: None,
            timings: None,
            finish_reasons: Vec::new(),
            streamed_chars: 0,
            reasoning_only_chars: 0,
            reasoning_only_char_limit,
            tags: ReasoningTags::new(split_reasoning_tags),
            t_first: None,
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

    /// Emit and fold a reasoning fragment. Returns `true` at the configured
    /// reasoning-only cutoff.
    fn handle_reasoning(&mut self, rc: &str, ui: &mut dyn UserInterface) -> bool {
        if !rc.is_empty() {
            ui.stream(StreamKind::Reasoning, rc.to_string());
            self.reasoning_parts.push(rc.to_string());
        }
        if self.no_output_yet() {
            self.reasoning_only_chars += rc.len();
        }
        self.reasoning_only_char_limit > 0
            && self.no_output_yet()
            && self.reasoning_only_chars >= self.reasoning_only_char_limit
    }

    /// Emit and fold an answer-content fragment.
    fn handle_content(&mut self, c: &str, ui: &mut dyn UserInterface) {
        if !c.is_empty() {
            ui.stream(StreamKind::Assistant, c.to_string());
            self.content_parts.push(c.to_string());
            self.streamed_chars += c.len() as u64;
        }
    }

    /// Fold the tool-call fragments in one chunk into the accumulator.
    fn handle_tool_calls(&mut self, chunk: &StreamChunk) {
        for tc in &chunk.tool_calls {
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

    /// Fold the thinking fragments in one chunk into their blocks.
    fn handle_thinking(&mut self, chunk: &StreamChunk) {
        for delta in &chunk.thinking {
            self.thinking.entry(delta.index).or_default().apply(delta);
        }
    }

    /// Fold and emit one chunk. A returned result is terminal: the caller must
    /// stop polling the HTTP stream and return it immediately.
    pub(crate) fn push(
        &mut self,
        chunk: &StreamChunk,
        t0: Instant,
        ui: &mut dyn UserInterface,
    ) -> Option<StreamResult> {
        self.note_meta(chunk, t0);
        if let Some(reasoning) = &chunk.reasoning_content
            && self.handle_reasoning(reasoning, ui)
        {
            return Some(self.stall(ui));
        }
        // Content is divided before it is folded, so an endpoint that wrapped
        // its deliberation in tags reaches the same two channels as one that
        // reported it in `reasoning_content`.
        if let Some(content) = &chunk.content {
            let split = self.tags.split(content);
            if !split.reasoning.is_empty() && self.handle_reasoning(&split.reasoning, ui) {
                return Some(self.stall(ui));
            }
            self.handle_content(&split.content, ui);
        }
        if !chunk.tool_calls.is_empty() {
            self.handle_tool_calls(chunk);
        }
        if !chunk.thinking.is_empty() {
            self.handle_thinking(chunk);
        }
        None
    }

    /// Report the reasoning-only cutoff and hand back what was deliberated.
    fn stall(&mut self, ui: &mut dyn UserInterface) -> StreamResult {
        ui.finish_stream();
        ui.message(
            MessageKind::Warning,
            format!(
                "REASONING-ONLY LIMIT - {} chars; cutting",
                abbr(self.reasoning_only_chars as u64)
            ),
        );
        StreamResult::ReasoningStall {
            chars: self.reasoning_only_chars,
            reasoning_parts: take(&mut self.reasoning_parts),
        }
    }

    /// Complete a normally exhausted SSE stream and return its accumulated
    /// model/tool data.
    ///
    /// A stream that ended mid-tag still has text held back waiting for the rest
    /// of it, and that text is the model's, so it is released here rather than
    /// dropped. The cutoff is not consulted: the stream is already over.
    pub(crate) fn finish(mut self, ui: &mut dyn UserInterface) -> StreamResult {
        let last = self.tags.flush();
        self.handle_reasoning(&last.reasoning, ui);
        self.handle_content(&last.content, ui);
        ui.finish_stream();
        StreamResult::Done(Box::new(self.into_accumulated()))
    }

    fn into_accumulated(self) -> Accumulated {
        Accumulated {
            content_parts: self.content_parts,
            tool_calls: self.tool_calls,
            reasoning_parts: self.reasoning_parts,
            thinking_blocks: self
                .thinking
                .into_values()
                .filter_map(ThinkingAccum::into_block)
                .collect(),
            usage: self.usage,
            timings: self.timings,
            finish_reasons: self.finish_reasons,
            streamed_chars: self.streamed_chars,
            reasoning_only_chars: self.reasoning_only_chars,
            t_first: self.t_first,
        }
    }
}

#[cfg(test)]
mod tests;
