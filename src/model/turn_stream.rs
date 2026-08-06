//! Incremental accumulation for a model turn. Each SSE chunk updates the fold
//! and emits typed UI deltas as soon as it arrives.

use std::collections::HashMap;
use std::mem::take;
use std::time::Instant;

use crate::metrics::abbr;
use crate::model::ModelConfig;
use crate::model::stream::{StreamChunk, Timings, Usage};
use crate::model::turn_dispatch::ToolCallAccum;
use crate::term::{MessageKind, StreamKind, UserInterface};

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

/// Mutable fold state updated once per live SSE chunk.
pub(crate) struct StreamAccumulator {
    content_parts: Vec<String>,
    tool_calls: HashMap<u32, ToolCallAccum>,
    reasoning_parts: Vec<String>,
    usage: Option<Usage>,
    timings: Option<Timings>,
    finish_reasons: Vec<String>,
    streamed_chars: u64,
    reasoning_only_chars: usize,
    t_first: Option<f64>,
}

impl StreamAccumulator {
    #[must_use]
    pub(crate) fn new() -> Self {
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
    fn handle_reasoning(
        &mut self,
        rc: &str,
        config: &ModelConfig,
        ui: &mut dyn UserInterface,
    ) -> bool {
        if !rc.is_empty() {
            ui.stream(StreamKind::Reasoning, rc.to_string());
            self.reasoning_parts.push(rc.to_string());
        }
        if self.no_output_yet() {
            self.reasoning_only_chars += rc.len();
        }
        config.reasoning_only_char_limit > 0
            && self.no_output_yet()
            && self.reasoning_only_chars >= config.reasoning_only_char_limit
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

    /// Fold and emit one chunk. A returned result is terminal: the caller must
    /// stop polling the HTTP stream and return it immediately.
    pub(crate) fn push(
        &mut self,
        chunk: &StreamChunk,
        config: &ModelConfig,
        t0: Instant,
        ui: &mut dyn UserInterface,
    ) -> Option<StreamResult> {
        self.note_meta(chunk, t0);
        if let Some(reasoning) = &chunk.reasoning_content
            && self.handle_reasoning(reasoning, config, ui)
        {
            ui.finish_stream();
            ui.message(
                MessageKind::Warning,
                format!(
                    "REASONING-ONLY LIMIT - {} chars; cutting",
                    abbr(self.reasoning_only_chars as u64)
                ),
            );
            return Some(StreamResult::ReasoningStall {
                chars: self.reasoning_only_chars,
                reasoning_parts: take(&mut self.reasoning_parts),
            });
        }
        if let Some(content) = &chunk.content {
            self.handle_content(content, ui);
        }
        if !chunk.tool_calls.is_empty() {
            self.handle_tool_calls(chunk);
        }
        None
    }

    /// Complete a normally exhausted SSE stream and return its accumulated
    /// model/tool data.
    pub(crate) fn finish(self, ui: &mut dyn UserInterface) -> StreamResult {
        ui.finish_stream();
        StreamResult::Done(Box::new(self.into_accumulated()))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::stream::ToolCallDelta;
    use crate::risk::ApprovalChoice;
    use crate::term::OutputEvent;
    use tokio_util::sync::CancellationToken;

    #[derive(Default)]
    struct RecordingUi {
        events: Vec<OutputEvent>,
    }

    impl UserInterface for RecordingUi {
        fn emit(&mut self, event: OutputEvent) {
            self.events.push(event);
        }

        fn start_activity(&mut self, _label: &str) -> CancellationToken {
            CancellationToken::new()
        }

        fn stop_activity(&mut self) {}

        fn approve(&mut self, _prompt: &str) -> ApprovalChoice {
            ApprovalChoice::No
        }
    }

    fn finish(accumulator: StreamAccumulator, ui: &mut RecordingUi) -> super::Accumulated {
        match accumulator.finish(ui) {
            StreamResult::Done(result) => *result,
            StreamResult::ReasoningStall { .. } => panic!("stream must finish"),
        }
    }

    fn push(
        accumulator: &mut StreamAccumulator,
        chunk: &StreamChunk,
        config: &ModelConfig,
        started: Instant,
        ui: &mut RecordingUi,
    ) {
        assert!(accumulator.push(chunk, config, started, ui).is_none());
    }

    #[test]
    fn emits_live_reasoning_and_assistant_deltas() {
        let mut ui = RecordingUi::default();
        let mut accumulator = StreamAccumulator::new();
        let config = ModelConfig::default();
        let started = Instant::now();

        let reasoning = StreamChunk {
            reasoning_content: Some("plan".to_string()),
            ..StreamChunk::default()
        };
        let answer = StreamChunk {
            content: Some("done".to_string()),
            ..StreamChunk::default()
        };
        push(&mut accumulator, &reasoning, &config, started, &mut ui);
        push(&mut accumulator, &answer, &config, started, &mut ui);

        let result = finish(accumulator, &mut ui);
        assert_eq!(result.reasoning_parts, ["plan"]);
        assert_eq!(result.content_parts, ["done"]);
        assert!(result.t_first.is_some());
        assert!(matches!(
            ui.events.as_slice(),
            [
                OutputEvent::Stream {
                    kind: StreamKind::Reasoning,
                    ..
                },
                OutputEvent::Stream {
                    kind: StreamKind::Assistant,
                    ..
                },
                OutputEvent::StreamFinished
            ]
        ));
    }

    #[test]
    fn preserves_whitespace_only_content_deltas() {
        let mut ui = RecordingUi::default();
        let mut accumulator = StreamAccumulator::new();
        let config = ModelConfig::default();
        for content in ["hello", " ", "world\n\n"] {
            let chunk = StreamChunk {
                content: Some(content.to_string()),
                ..StreamChunk::default()
            };
            assert!(
                accumulator
                    .push(&chunk, &config, Instant::now(), &mut ui)
                    .is_none()
            );
        }
        let result = finish(accumulator, &mut ui);
        assert_eq!(result.content_parts.join(""), "hello world\n\n");
    }

    #[test]
    fn merges_tool_call_deltas_incrementally() {
        let mut ui = RecordingUi::default();
        let mut accumulator = StreamAccumulator::new();
        let config = ModelConfig::default();
        for arguments in ["{\"path\":", "\"README.md\"}"] {
            let chunk = StreamChunk {
                tool_calls: vec![ToolCallDelta {
                    index: 0,
                    id: Some("call-1".to_string()),
                    name: Some("read_file".to_string()),
                    arguments: Some(arguments.to_string()),
                }],
                ..StreamChunk::default()
            };
            assert!(
                accumulator
                    .push(&chunk, &config, Instant::now(), &mut ui)
                    .is_none()
            );
        }

        let result = finish(accumulator, &mut ui);
        assert_eq!(result.tool_calls[&0].args, "{\"path\":\"README.md\"}");
    }

    #[test]
    fn reasoning_limit_returns_terminal_result() {
        let mut ui = RecordingUi::default();
        let mut accumulator = StreamAccumulator::new();
        let config = ModelConfig {
            reasoning_only_char_limit: 4,
            ..ModelConfig::default()
        };
        let chunk = StreamChunk {
            reasoning_content: Some("think".to_string()),
            ..StreamChunk::default()
        };

        let result = accumulator
            .push(&chunk, &config, Instant::now(), &mut ui)
            .expect("limit must stop the stream");
        let StreamResult::ReasoningStall { chars, .. } = result else {
            panic!("expected reasoning stall");
        };
        assert_eq!(chars, 5);
        assert!(
            ui.events
                .iter()
                .any(|event| matches!(event, OutputEvent::StreamFinished))
        );
    }
}
