//! Anthropic Messages API SSE decoder.
//!
//! Maps Anthropic's event stream onto the same [`StreamChunk`] the
//! `OpenAI`-compatible decoder produces, so nothing downstream of the client
//! needs to know which protocol served the turn. Two mappings carry that:
//!
//! * `stop_reason` is normalized into `OpenAI` `finish_reason` vocabulary, which
//!   keeps the `saw_finish` EOF guard and `turn_finalize`'s `.contains("length")`
//!   truncation check working untouched.
//! * `thinking_delta` lands in `reasoning_content`, the field the llama.cpp /
//!   `DeepSeek` reasoning path already uses.
//!
//! Anthropic frames every event with both an `event:` name and a `"type"` field
//! inside the `data:` payload. Only the payload is read, so the framing layer
//! can keep discarding `event:` lines.

use serde_json::Value;

use crate::model::client::sse::SseDecoder;
use crate::model::stream::{
    PromptTokensDetails, SseDecodeError, SseLine, StreamChunk, ToolCallDelta, Usage,
};

/// Decoder for Anthropic Messages API streams.
///
/// Stateful: token usage is split across two events (`message_start` carries the
/// input side, `message_delta` the output side) while `StreamAccumulator` keeps
/// only the last `usage` it sees. The input counters are therefore held here and
/// emitted once, merged, on `message_delta`.
#[derive(Debug, Default)]
pub(crate) struct AnthropicDecoder {
    input: u64,
    cache_read: u64,
    cache_creation: u64,
}

impl AnthropicDecoder {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Stash the input-side token counts from `message_start`.
    fn note_input_usage(&mut self, event: &Value) {
        let Some(usage) = event.pointer("/message/usage") else {
            return;
        };
        self.input = u64_field(usage, "input_tokens");
        self.cache_read = u64_field(usage, "cache_read_input_tokens");
        self.cache_creation = u64_field(usage, "cache_creation_input_tokens");
    }

    /// Combine the stashed input counts with `message_delta`'s output count.
    ///
    /// `normalize_usage` derives `input_tokens = prompt_tokens - cached_tokens`,
    /// but Anthropic's `input_tokens` *already excludes* cached tokens. So
    /// `prompt_tokens` is the re-inflated total and only `cache_read` is
    /// reported as cached - cache *creation* is billed as fresh input.
    fn merged_usage(&self, event: &Value) -> Usage {
        Usage {
            prompt_tokens: self
                .input
                .saturating_add(self.cache_read)
                .saturating_add(self.cache_creation),
            completion_tokens: event
                .pointer("/usage/output_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            prompt_tokens_details: Some(PromptTokensDetails {
                cached_tokens: self.cache_read,
            }),
            // Anthropic bills thinking inside output_tokens without a separate
            // breakdown. Reporting it here would make `normalize_usage`
            // subtract it out and under-report the real output.
            output_tokens_details: None,
        }
    }

    fn message_delta(&self, event: &Value) -> Result<SseLine, SseDecodeError> {
        let delta = event.get("delta");
        let stop = delta
            .and_then(|d| d.get("stop_reason"))
            .and_then(Value::as_str);
        if stop == Some("refusal") {
            return Err(SseDecodeError::Provider(refusal_message(delta)));
        }
        Ok(SseLine::Chunk(StreamChunk {
            finish_reason: stop.map(normalize_stop_reason),
            usage: Some(self.merged_usage(event)),
            ..StreamChunk::default()
        }))
    }

    fn dispatch(&mut self, event: &Value) -> Result<SseLine, SseDecodeError> {
        match event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "message_start" => {
                self.note_input_usage(event);
                Ok(SseLine::Ignore)
            }
            "content_block_start" => Ok(block_start(event)),
            "content_block_delta" => Ok(block_delta(event)),
            "message_delta" => self.message_delta(event),
            "message_stop" => Ok(SseLine::Done),
            // `ping`, `content_block_stop`, and any event type Anthropic adds
            // later. Ignoring unknown types keeps the decoder forward-compatible.
            _ => Ok(SseLine::Ignore),
        }
    }
}

impl SseDecoder for AnthropicDecoder {
    fn decode(&mut self, data: &str) -> Result<SseLine, SseDecodeError> {
        // Parse before touching state. The framing layer speculatively decodes
        // each `data:` field before buffering it, so one field of a fragmented
        // event reaches us and must fail here without mutating the counters.
        let event: Value = serde_json::from_str(data)?;
        if let Some(error) = event.get("error") {
            return Err(SseDecodeError::Provider(error_message(error)));
        }
        self.dispatch(&event)
    }
}

/// Map an Anthropic `stop_reason` onto `OpenAI` `finish_reason` vocabulary.
///
/// Unknown values pass through verbatim rather than collapsing to `"stop"`, so
/// a future stop reason cannot silently look like a clean finish.
fn normalize_stop_reason(raw: &str) -> String {
    match raw {
        // `pause_turn` only arises from server-side tools, which afi does not
        // declare; treating it as a clean stop is the safe reading.
        "end_turn" | "stop_sequence" | "pause_turn" => "stop",
        "tool_use" => "tool_calls",
        // `.contains("length")` is what drives the forced-final path.
        "max_tokens" | "model_context_window_exceeded" => "length",
        other => other,
    }
    .to_string()
}

/// `content_block_start`. Only `tool_use` blocks carry information here - text
/// and thinking blocks always start empty and stream their content as deltas.
fn block_start(event: &Value) -> SseLine {
    let Some(block) = event.get("content_block") else {
        return SseLine::Ignore;
    };
    if block.get("type").and_then(Value::as_str) != Some("tool_use") {
        return SseLine::Ignore;
    }
    SseLine::Chunk(StreamChunk {
        tool_calls: vec![ToolCallDelta {
            index: index_of(event),
            id: str_field(block, "id"),
            name: str_field(block, "name"),
            arguments: None,
        }],
        ..StreamChunk::default()
    })
}

/// `content_block_delta`. The delta self-describes its kind, so no per-index
/// block-type bookkeeping is needed.
fn block_delta(event: &Value) -> SseLine {
    let Some(delta) = event.get("delta") else {
        return SseLine::Ignore;
    };
    match delta
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "text_delta" => text_chunk(str_field(delta, "text")),
        "thinking_delta" => reasoning_chunk(str_field(delta, "thinking")),
        "input_json_delta" => args_chunk(index_of(event), str_field(delta, "partial_json")),
        // `signature_delta` carries a thinking-block signature. afi never
        // replays thinking blocks, so it is dropped.
        _ => SseLine::Ignore,
    }
}

fn text_chunk(text: Option<String>) -> SseLine {
    match text {
        Some(content) => SseLine::Chunk(StreamChunk {
            content: Some(content),
            ..StreamChunk::default()
        }),
        None => SseLine::Ignore,
    }
}

fn reasoning_chunk(thinking: Option<String>) -> SseLine {
    match thinking {
        Some(reasoning) => SseLine::Chunk(StreamChunk {
            reasoning_content: Some(reasoning),
            ..StreamChunk::default()
        }),
        None => SseLine::Ignore,
    }
}

fn args_chunk(index: u32, partial: Option<String>) -> SseLine {
    match partial {
        Some(arguments) => SseLine::Chunk(StreamChunk {
            tool_calls: vec![ToolCallDelta {
                index,
                id: None,
                name: None,
                arguments: Some(arguments),
            }],
            ..StreamChunk::default()
        }),
        None => SseLine::Ignore,
    }
}

/// Anthropic's `content_block` index counts *all* blocks, so a `[text, tool_use]`
/// response gives the tool index 1. `ToolCallAccum` keys on this verbatim and
/// `order_tool_calls` sorts by it, so gaps are harmless.
fn index_of(event: &Value) -> u32 {
    event
        .get("index")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(0)
}

fn str_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(String::from)
}

fn u64_field(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// `{"type":"error","error":{"type":"overloaded_error","message":"..."}}`
fn error_message(error: &Value) -> String {
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| error.as_str());
    match (error.get("type").and_then(Value::as_str), message) {
        (Some(kind), Some(text)) => format!("{kind}: {text}"),
        (None, Some(text)) => text.to_string(),
        _ => error.to_string(),
    }
}

/// A refusal is surfaced as a provider error rather than a clean stop: mapping
/// it to `"stop"` with empty content would send the turn loop through the
/// empty-turn nudge, its retries, and a forced-final, all of which would be
/// refused again.
fn refusal_message(delta: Option<&Value>) -> String {
    let details = delta.and_then(|d| d.get("stop_details"));
    let category = details
        .and_then(|d| d.get("category"))
        .and_then(Value::as_str)
        .unwrap_or("unspecified");
    let explanation = details
        .and_then(|d| d.get("explanation"))
        .and_then(Value::as_str)
        .unwrap_or("the model declined this request");
    format!("refusal ({category}): {explanation}")
}

#[cfg(test)]
mod tests;
