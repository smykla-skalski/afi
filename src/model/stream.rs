//! SSE stream parsing and usage normalization.
//!
//! Parses `data: {...}\n\n` SSE events from the OpenAI-compatible streaming
//! API, extracts content / `tool_calls` / `reasoning_content` / usage from each
//! chunk, and normalizes token usage from both the `OpenAI` `usage` object and
//! the llama.cpp `timings` extra.

use serde::{Deserialize, Serialize};
use serde_json::Value;

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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PromptTokensDetails {
    pub cached_tokens: u64,
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

/// Normalized token usage: input (minus cache), output (minus reasoning),
/// `cache_read`, reasoning. Matches the `OpenAI` usage convention.
#[derive(Debug, Clone, Default)]
pub struct NormalizedUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
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
            reasoning_tokens: 0,
        });
    }

    if let Some(u) = usage {
        let cache_n = u
            .prompt_tokens_details
            .as_ref()
            .map_or(0, |d| d.cached_tokens);
        let reasoning_n = u
            .output_tokens_details
            .as_ref()
            .map_or(0, |d| d.reasoning_tokens);
        return Some(NormalizedUsage {
            input_tokens: u.prompt_tokens.saturating_sub(cache_n),
            output_tokens: u.completion_tokens.saturating_sub(reasoning_n),
            cache_read_tokens: cache_n,
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
            reasoning_tokens: 0,
        });
    }

    None
}

/// Parse a single `data: {...}` SSE line into a `StreamChunk`. Returns
/// `None` for `data: [DONE]` or blank lines.
#[must_use]
pub fn parse_sse_line(line: &str) -> Option<StreamChunk> {
    let line = line.trim();
    if line.is_empty() || line.starts_with(':') {
        return None;
    }
    let json_str = line.strip_prefix("data: ")?;
    if json_str.trim() == "[DONE]" {
        return None;
    }
    let v: Value = serde_json::from_str(json_str).ok()?;

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
    Some(chunk)
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
    body.split("\n\n")
        .flat_map(|event| event.lines().filter_map(parse_sse_line).collect::<Vec<_>>())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_usage_openai() {
        let usage = Usage {
            prompt_tokens: 1000,
            completion_tokens: 500,
            prompt_tokens_details: Some(PromptTokensDetails { cached_tokens: 200 }),
            output_tokens_details: Some(OutputTokensDetails {
                reasoning_tokens: 50,
            }),
        };
        let n = normalize_usage(Some(&usage), None, 0).unwrap();
        assert_eq!(n.input_tokens, 800); // 1000 - 200 cache
        assert_eq!(n.output_tokens, 450); // 500 - 50 reasoning
        assert_eq!(n.cache_read_tokens, 200);
        assert_eq!(n.reasoning_tokens, 50);
    }

    #[test]
    fn normalize_usage_llamacpp_timings() {
        let timings = Timings {
            prompt_n: 1000,
            predicted_n: 500,
            cache_n: 200,
        };
        let n = normalize_usage(None, Some(&timings), 0).unwrap();
        assert_eq!(n.input_tokens, 800); // 1000 - 200 cache
        assert_eq!(n.output_tokens, 500);
        assert_eq!(n.cache_read_tokens, 200);
        assert_eq!(n.reasoning_tokens, 0);
    }

    #[test]
    fn normalize_usage_fallback_chars() {
        let n = normalize_usage(None, None, 8000).unwrap();
        assert_eq!(n.output_tokens, 2000); // 8000 / 4
        assert_eq!(n.input_tokens, 0);
    }

    #[test]
    fn normalize_usage_none() {
        assert!(normalize_usage(None, None, 0).is_none());
    }

    #[test]
    fn parse_sse_line_content() {
        let line = r#"data: {"choices":[{"delta":{"content":"hello"},"finish_reason":null}]}"#;
        let chunk = parse_sse_line(line).unwrap();
        assert_eq!(chunk.content, Some("hello".to_string()));
    }

    #[test]
    fn parse_sse_line_done() {
        let line = "data: [DONE]";
        assert!(parse_sse_line(line).is_none());
    }

    #[test]
    fn parse_sse_line_tool_calls() {
        let line = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read_file","arguments":"{\"path\":"}}]}}]}"#;
        let chunk = parse_sse_line(line).unwrap();
        assert_eq!(chunk.tool_calls.len(), 1);
        assert_eq!(chunk.tool_calls[0].id, Some("call_1".to_string()));
        assert_eq!(chunk.tool_calls[0].name, Some("read_file".to_string()));
    }

    #[test]
    fn parse_sse_line_reasoning() {
        let line = r#"data: {"choices":[{"delta":{"reasoning_content":"thinking..."}}]}"#;
        let chunk = parse_sse_line(line).unwrap();
        assert_eq!(chunk.reasoning_content, Some("thinking...".to_string()));
    }

    #[test]
    fn parse_sse_line_usage() {
        let line = r#"data: {"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":50}}"#;
        let chunk = parse_sse_line(line).unwrap();
        assert!(chunk.usage.is_some());
        assert_eq!(chunk.usage.unwrap().prompt_tokens, 100);
    }

    #[test]
    fn parse_sse_body_multiple_chunks() {
        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"b\"}}]}\n\ndata: [DONE]\n\n";
        let chunks = parse_sse_body(body);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].content, Some("a".to_string()));
        assert_eq!(chunks[1].content, Some("b".to_string()));
    }
}
