//! SSE stream parsing and usage normalization.
//!
//! Parses `data: {...}\n\n` SSE events from the OpenAI-compatible streaming
//! API, extracts content / `tool_calls` / `reasoning_content` / usage from each
//! chunk, and normalizes token usage from both the `OpenAI` `usage` object and
//! the llama.cpp `timings` extra.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// A parsed SSE chunk from the streaming chat completions API.
#[derive(Debug, Clone, Default)]
pub struct StreamChunk {
    pub content: Option<String>,
    pub reasoning_content: Option<String>,
    pub tool_calls: Vec<ToolCallDelta>,
    pub finish_reason: Option<String>,
    pub usage: Option<Usage>,
    pub timings: Option<Timings>,
}

/// Result of decoding one physical SSE line.
#[derive(Debug)]
pub(crate) enum SseLine {
    Chunk(StreamChunk),
    Done,
    Ignore,
}

#[derive(Debug, Error)]
pub(crate) enum SseDecodeError {
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("provider error: {0}")]
    Provider(String),
}

/// A streamed tool-call delta.
#[derive(Debug, Clone, Default)]
pub struct ToolCallDelta {
    pub index: u32,
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments: Option<String>,
}

/// The standard `OpenAI` usage object.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_tokens_details: Option<PromptTokensDetails>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens_details: Option<OutputTokensDetails>,
}

/// Subsets of `prompt_tokens`, following `OpenAI`'s convention that a detail
/// here is part of the prompt total rather than an addition to it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PromptTokensDetails {
    pub cached_tokens: u64,
    /// Prompt tokens written into the cache rather than read from it, which
    /// Anthropic bills above both a read and plain input. No `OpenAI`-compatible
    /// provider reports one, so it defaults to 0 rather than being guessed at.
    #[serde(default)]
    pub cache_write_tokens: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OutputTokensDetails {
    pub reasoning_tokens: u64,
}

/// The llama.cpp timings object (alternative to `usage`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Timings {
    pub prompt_n: u64,
    pub predicted_n: u64,
    pub cache_n: u64,
}

/// Normalized token usage: input (minus both cache subsets), output (minus
/// reasoning), `cache_read`, `cache_write`, reasoning.
///
/// The five counts are deliberately disjoint, so a caller pricing a run can
/// multiply each by its own rate and sum. Cache reads and writes are split
/// because they are not billed alike - Anthropic charges a write above base
/// input and a read far below it, so folding either into `input_tokens` puts
/// tokens at the wrong price.
#[derive(Debug, Clone, Default)]
pub struct NormalizedUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    /// Prompt tokens written into the cache. Only the Anthropic path reports
    /// this; every other provider leaves it 0.
    pub cache_write_tokens: u64,
    pub reasoning_tokens: u64,
}

/// Pull a normalized usage dict from whichever the server gave us: the
/// standard `usage` object (OpenAI/Z.ai) or the llama.cpp `timings` extra.
/// Returns `None` if neither has anything to say.
///
/// When neither is available, a rough estimate is derived from `fallback_chars`
/// (client-side char count of streamed content + tool-call arguments).
#[must_use]
pub fn normalize_usage(
    usage: Option<&Usage>,
    timings: Option<&Timings>,
    fallback_chars: u64,
) -> Option<NormalizedUsage> {
    // llama.cpp: no `usage`, but a `timings` object with prompt_n / predicted_n / cache_n.
    if usage.is_none()
        && let Some(t) = timings
        && t.predicted_n > 0
    {
        return Some(NormalizedUsage {
            input_tokens: t.prompt_n.saturating_sub(t.cache_n),
            output_tokens: t.predicted_n,
            cache_read_tokens: t.cache_n,
            // `cache_n` counts the reused prefix llama.cpp already held, which
            // is a read. It has no separate figure for populating that prefix,
            // and reporting one here would be an invention.
            cache_write_tokens: 0,
            reasoning_tokens: 0,
        });
    }

    if let Some(u) = usage {
        let (cache_read_n, cache_write_n) = u
            .prompt_tokens_details
            .as_ref()
            .map_or((0, 0), |d| (d.cached_tokens, d.cache_write_tokens));
        let reasoning_n = u
            .output_tokens_details
            .as_ref()
            .map_or(0, |d| d.reasoning_tokens);
        return Some(NormalizedUsage {
            // Both cache counts are subsets of `prompt_tokens`, so both come
            // out to leave the tokens billed at the plain input rate.
            input_tokens: u
                .prompt_tokens
                .saturating_sub(cache_read_n)
                .saturating_sub(cache_write_n),
            output_tokens: u.completion_tokens.saturating_sub(reasoning_n),
            cache_read_tokens: cache_read_n,
            cache_write_tokens: cache_write_n,
            reasoning_tokens: reasoning_n,
        });
    }

    // Fallback: estimate from client-side char count (~4 chars/token).
    if fallback_chars > 0 {
        let estimated = (fallback_chars / 4).max(1);
        return Some(NormalizedUsage {
            input_tokens: 0,
            output_tokens: estimated,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
        });
    }

    None
}

/// Decode one physical SSE line, preserving malformed JSON as an error for the
/// live HTTP stream. Both `data:{...}` and `data: {...}` are accepted.
pub(crate) fn decode_sse_line(line: &str) -> Result<SseLine, SseDecodeError> {
    let line = line.trim();
    if line.is_empty() || line.starts_with(':') {
        return Ok(SseLine::Ignore);
    }
    let Some(json_str) = line.strip_prefix("data:") else {
        return Ok(SseLine::Ignore);
    };
    decode_sse_data(json_str)
}

/// Decode the joined `data` fields from one SSE event.
pub(crate) fn decode_sse_data(data: &str) -> Result<SseLine, SseDecodeError> {
    let data = data.trim_start();
    if data.trim() == "[DONE]" {
        return Ok(SseLine::Done);
    }
    if data.trim().is_empty() {
        return Ok(SseLine::Ignore);
    }
    let v: Value = serde_json::from_str(data)?;
    if let Some(error) = v.get("error") {
        return Err(SseDecodeError::Provider(provider_error_message(error)));
    }

    let mut chunk = StreamChunk::default();
    if let Some(choice) = v
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|c| c.first())
    {
        apply_choice(&mut chunk, choice);
    }
    // Usage / timings ride on the final chunk.
    if let Some(u) = v.get("usage") {
        chunk.usage = serde_json::from_value(u.clone()).ok();
    }
    if let Some(t) = v.get("timings") {
        chunk.timings = serde_json::from_value(t.clone()).ok();
    }
    Ok(SseLine::Chunk(chunk))
}

fn provider_error_message(error: &Value) -> String {
    error
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| error.as_str())
        .map_or_else(|| error.to_string(), str::to_string)
}

/// Parse a single `data: {...}` SSE line into a `StreamChunk`. Returns
/// `None` for terminal, ignored, or malformed lines.
#[must_use]
pub fn parse_sse_line(line: &str) -> Option<StreamChunk> {
    match decode_sse_line(line).ok()? {
        SseLine::Chunk(chunk) => Some(chunk),
        SseLine::Done | SseLine::Ignore => None,
    }
}

/// Fold one `choices[0]` entry (delta `content`/`reasoning`/`tool_calls` +
/// `finish_reason`) into `chunk`.
fn apply_choice(chunk: &mut StreamChunk, choice: &Value) {
    if let Some(delta) = choice.get("delta") {
        chunk.content = delta
            .get("content")
            .and_then(|c| c.as_str())
            .map(String::from);
        chunk.reasoning_content = delta
            .get("reasoning_content")
            .and_then(|c| c.as_str())
            .map(String::from);
        if let Some(tcs) = delta.get("tool_calls").and_then(|t| t.as_array()) {
            chunk.tool_calls = tcs.iter().map(parse_tool_delta).collect();
        }
    }
    chunk.finish_reason = choice
        .get("finish_reason")
        .and_then(|f| f.as_str())
        .map(String::from);
}

/// Parse one streamed `tool_calls` delta entry.
fn parse_tool_delta(tc: &Value) -> ToolCallDelta {
    let mut delta_t = ToolCallDelta {
        index: tc
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(0),
        ..Default::default()
    };
    delta_t.id = tc.get("id").and_then(|i| i.as_str()).map(String::from);
    if let Some(func) = tc.get("function") {
        delta_t.name = func.get("name").and_then(|n| n.as_str()).map(String::from);
        delta_t.arguments = func
            .get("arguments")
            .and_then(|a| a.as_str())
            .map(String::from);
    }
    delta_t
}

/// Parse a full SSE response body into a Vec of `StreamChunks`.
#[must_use]
pub fn parse_sse_body(body: &str) -> Vec<StreamChunk> {
    let mut chunks = Vec::new();
    let mut data = Vec::new();
    for line in body.lines().chain([""]) {
        if let Some(value) = line.strip_prefix("data:") {
            data.push(value.trim_start());
            continue;
        }
        if !line.is_empty() || data.is_empty() {
            continue;
        }
        match decode_sse_data(&data.join("\n")) {
            Ok(SseLine::Chunk(chunk)) => chunks.push(chunk),
            Ok(SseLine::Done) => return chunks,
            Ok(SseLine::Ignore) | Err(_) => {}
        }
        data.clear();
    }
    chunks
}

#[cfg(test)]
mod tests;
