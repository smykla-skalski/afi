//! Fold tests: live deltas, incremental tool-call merging, the reasoning-only
//! cut, and the thinking blocks the Anthropic path has to replay.

mod reasoning_tags;

use super::*;
use crate::model::stream::{ThinkingDelta, ToolCallDelta};
use crate::risk::ApprovalChoice;
use crate::term::OutputEvent;
use tokio_util::sync::CancellationToken;

/// The stock reasoning-only cut, matching `ModelConfig::default()`.
const DEFAULT_LIMIT: usize = 36000;

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

fn push(accumulator: &mut StreamAccumulator, chunk: &StreamChunk, ui: &mut RecordingUi) {
    assert!(accumulator.push(chunk, Instant::now(), ui).is_none());
}

#[test]
fn emits_live_reasoning_and_assistant_deltas() {
    let mut ui = RecordingUi::default();
    let mut accumulator = StreamAccumulator::new(DEFAULT_LIMIT, true);

    let reasoning = StreamChunk {
        reasoning_content: Some("plan".to_string()),
        ..StreamChunk::default()
    };
    let answer = StreamChunk {
        content: Some("done".to_string()),
        ..StreamChunk::default()
    };
    push(&mut accumulator, &reasoning, &mut ui);
    push(&mut accumulator, &answer, &mut ui);

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
    let mut accumulator = StreamAccumulator::new(DEFAULT_LIMIT, true);
    for content in ["hello", " ", "world\n\n"] {
        let chunk = StreamChunk {
            content: Some(content.to_string()),
            ..StreamChunk::default()
        };
        push(&mut accumulator, &chunk, &mut ui);
    }
    let result = finish(accumulator, &mut ui);
    assert_eq!(result.content_parts.join(""), "hello world\n\n");
}

#[test]
fn merges_tool_call_deltas_incrementally() {
    let mut ui = RecordingUi::default();
    let mut accumulator = StreamAccumulator::new(DEFAULT_LIMIT, true);
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
        push(&mut accumulator, &chunk, &mut ui);
    }

    let result = finish(accumulator, &mut ui);
    assert_eq!(result.tool_calls[&0].args, "{\"path\":\"README.md\"}");
}

#[test]
fn reasoning_limit_returns_terminal_result() {
    let mut ui = RecordingUi::default();
    let mut accumulator = StreamAccumulator::new(4, true);
    let chunk = StreamChunk {
        reasoning_content: Some("think".to_string()),
        ..StreamChunk::default()
    };

    let result = accumulator
        .push(&chunk, Instant::now(), &mut ui)
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

#[test]
fn a_zero_limit_never_cuts() {
    // What the Anthropic path uses while thinking is on: the reasoning is
    // server-side and bounded by max_tokens, so a cut would be a false
    // positive.
    let mut ui = RecordingUi::default();
    let mut accumulator = StreamAccumulator::new(0, true);
    let chunk = StreamChunk {
        reasoning_content: Some("a very long deliberation".to_string()),
        ..StreamChunk::default()
    };
    for _ in 0..100 {
        push(&mut accumulator, &chunk, &mut ui);
    }
    assert!(finish(accumulator, &mut ui).reasoning_only_chars > 0);
}

// --- thinking blocks ------------------------------------------------------------

#[test]
fn assembles_a_signed_thinking_block_from_its_deltas() {
    let mut ui = RecordingUi::default();
    let mut accumulator = StreamAccumulator::new(DEFAULT_LIMIT, true);
    for text in ["let me ", "check the file"] {
        push(
            &mut accumulator,
            &StreamChunk {
                reasoning_content: Some(text.to_string()),
                thinking: vec![ThinkingDelta {
                    index: 0,
                    thinking: Some(text.to_string()),
                    ..ThinkingDelta::default()
                }],
                ..StreamChunk::default()
            },
            &mut ui,
        );
    }
    push(
        &mut accumulator,
        &StreamChunk {
            thinking: vec![ThinkingDelta {
                index: 0,
                signature: Some("sig".to_string()),
                ..ThinkingDelta::default()
            }],
            ..StreamChunk::default()
        },
        &mut ui,
    );

    let result = finish(accumulator, &mut ui);
    assert_eq!(
        result.thinking_blocks,
        vec![json!({
            "type": "thinking",
            "thinking": "let me check the file",
            "signature": "sig",
        })]
    );
}

#[test]
fn orders_blocks_by_content_index() {
    let mut ui = RecordingUi::default();
    let mut accumulator = StreamAccumulator::new(DEFAULT_LIMIT, true);
    // Arrive out of order; Anthropic's index is what restores document order.
    for (index, signature) in [(2u32, "second"), (0u32, "first")] {
        push(
            &mut accumulator,
            &StreamChunk {
                thinking: vec![ThinkingDelta {
                    index,
                    signature: Some(signature.to_string()),
                    ..ThinkingDelta::default()
                }],
                ..StreamChunk::default()
            },
            &mut ui,
        );
    }

    let result = finish(accumulator, &mut ui);
    let signatures: Vec<&str> = result
        .thinking_blocks
        .iter()
        .map(|block| block["signature"].as_str().unwrap())
        .collect();
    assert_eq!(signatures, ["first", "second"]);
}

#[test]
fn keeps_a_redacted_block_as_its_opaque_payload() {
    let mut ui = RecordingUi::default();
    let mut accumulator = StreamAccumulator::new(DEFAULT_LIMIT, true);
    push(
        &mut accumulator,
        &StreamChunk {
            thinking: vec![ThinkingDelta {
                index: 0,
                redacted: Some("encrypted".to_string()),
                ..ThinkingDelta::default()
            }],
            ..StreamChunk::default()
        },
        &mut ui,
    );

    let result = finish(accumulator, &mut ui);
    assert_eq!(
        result.thinking_blocks,
        vec![json!({"type": "redacted_thinking", "data": "encrypted"})]
    );
}

#[test]
fn drops_a_thinking_block_that_never_got_a_signature() {
    // A cut stream can leave one behind. Replaying it fails the whole next
    // request, so it is worth less than the text it holds.
    let mut ui = RecordingUi::default();
    let mut accumulator = StreamAccumulator::new(DEFAULT_LIMIT, true);
    push(
        &mut accumulator,
        &StreamChunk {
            reasoning_content: Some("half a thought".to_string()),
            thinking: vec![ThinkingDelta {
                index: 0,
                thinking: Some("half a thought".to_string()),
                ..ThinkingDelta::default()
            }],
            ..StreamChunk::default()
        },
        &mut ui,
    );

    let result = finish(accumulator, &mut ui);
    assert!(result.thinking_blocks.is_empty());
    // The text still reached the user; only the replay copy is dropped.
    assert_eq!(result.reasoning_parts, ["half a thought"]);
}

#[test]
fn an_openai_turn_produces_no_blocks() {
    let mut ui = RecordingUi::default();
    let mut accumulator = StreamAccumulator::new(DEFAULT_LIMIT, true);
    push(
        &mut accumulator,
        &StreamChunk {
            reasoning_content: Some("deepseek style".to_string()),
            ..StreamChunk::default()
        },
        &mut ui,
    );
    assert!(finish(accumulator, &mut ui).thinking_blocks.is_empty());
}
