//! Anthropic Messages API support.
//!
//! A second wire protocol alongside the `OpenAI`-compatible one. The seams are
//! narrow by design: history is translated at the boundary rather than stored
//! differently ([`messages`]), and the SSE decoder normalizes Anthropic's events
//! into the same `StreamChunk` ([`decode`]), so the turn loop, session store,
//! compression, and transcript printer are all protocol-agnostic.
//!
//! Sampling parameters are deliberately *not* forwarded. `temperature`,
//! `top_p`, and `top_k` are rejected outright by current Anthropic models, and
//! the recovery sampler's `min_p` / `repeat_penalty` / DRY knobs are
//! llama.cpp-only. Suppressing them here rather than in `ModelConfig` keeps the
//! choice per-source, so a llama.cpp source in the same session keeps its
//! recovery sampling.

use std::sync::LazyLock;
use std::time::Duration;

use futures::TryStreamExt;
use regex::Regex;
use reqwest::Client;
use serde_json::{Value, json};
use tokio_util::io::StreamReader;

use std::io;

use super::sse::decoded_stream;
use super::{
    ChatCompletionStream, ClientError, ReqwestClient, StreamRequest, limited_error_body,
    transport_error,
};
use crate::config::Source;
use crate::model::stream::{NormalizedUsage, PromptTokensDetails, Usage, normalize_usage};
use crate::model::usage_totals;

pub(crate) mod auth;
mod decode;
mod messages;
pub(crate) mod thinking;
mod tools;

pub(crate) use auth::TokenCache;
use decode::AnthropicDecoder;

/// Anthropic requires `max_tokens` on every request, and it caps thinking *plus*
/// visible text. The forced-final path asks for only 2048, which adaptive
/// thinking can consume entirely and return an empty answer, so floor it.
const MIN_MAX_TOKENS: u32 = 4096;
/// Used when the caller supplied no limit at all.
const DEFAULT_MAX_TOKENS: u32 = 16_000;
/// The floor once the request may actually think.
///
/// 4096 is room for an answer and not for the reasoning in front of it, which
/// is the same failure that floor was raised to prevent, one tier up: a
/// forced-final turn at 2048 becomes 4096, spends all of it thinking, and
/// returns no text. Thinking is on whenever a source configures it and whenever
/// `--effort` goes above `high`, so the floor has to move with it. A caller who
/// asked for less than the default gets the default; higher effort wants more
/// than that, and `AFI_MAX_TOKENS` is how you say so.
const MIN_MAX_TOKENS_THINKING: u32 = DEFAULT_MAX_TOKENS;

/// Body keys a source may set through its `EXTRA_BODY`.
///
/// An allowlist rather than a passthrough: Anthropic rejects unknown top-level
/// parameters, and the recovery sampler's keys must never reach it.
///
/// `thinking` is also configurable, but not through this list - it has three
/// states rather than two and decides whether history replays thinking blocks,
/// so it is resolved separately in [`thinking::resolve`].
const ALLOWED_EXTRA_KEYS: &[&str] = &[
    "output_config",
    "metadata",
    "stop_sequences",
    "service_tier",
];

/// Matches a trailing `/v1`, `/v2`, ... so a base url written with or without
/// the version suffix resolves to the same endpoint.
static VERSION_SUFFIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"/v\d+/?$").unwrap());

/// Strip any trailing version segment from a configured base url.
///
/// Sources conventionally include `/v1` (`https://api.together.xyz/v1`), so
/// appending `/v1/messages` blindly would produce `/v1/v1/messages`.
fn api_root(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    VERSION_SUFFIX
        .replace(trimmed, "")
        .trim_end_matches('/')
        .to_string()
}

fn messages_url(base_url: &str) -> String {
    format!("{}/v1/messages", api_root(base_url))
}

/// The workload-identity federation token-exchange endpoint.
fn token_url(base_url: &str) -> String {
    format!("{}/v1/oauth/token", api_root(base_url))
}

/// Everything needed to build a request body, gathered so neither builder needs
/// a long argument list.
struct BodyParams<'a> {
    model: &'a str,
    history: &'a [Value],
    tools: Option<&'a Value>,
    tool_choice: Option<&'a Value>,
    max_tokens: Option<u32>,
    extra_body: Option<&'a Value>,
    stream: bool,
}

fn build_body(params: &BodyParams<'_>) -> Value {
    let thinking = thinking::resolve(params.extra_body);
    let mode = thinking::mode(thinking.as_ref());
    let translated = messages::translate(params.history, mode);
    let mut body = json!({
        "model": params.model,
        "max_tokens": clamp_max_tokens(params.max_tokens, mode),
        "stream": params.stream,
        "messages": translated.messages,
    });
    // Absent only when the source asked for omission, which is what Claude
    // Fable 5 needs - it rejects an explicit `disabled` and always thinks.
    if let Some(value) = thinking {
        body["thinking"] = value;
    }
    if let Some(system) = &translated.system {
        body["system"] = cached_system(system);
    }
    if let Some(translated_tools) = params.tools.and_then(tools::translate_tools) {
        body["tools"] = translated_tools;
    }
    if let Some(choice) = params.tool_choice.and_then(tools::translate_tool_choice) {
        body["tool_choice"] = choice;
    }
    apply_allowed_extras(&mut body, params.extra_body);
    body
}

fn clamp_max_tokens(requested: Option<u32>, thinking: thinking::Thinking) -> u32 {
    let floor = if thinking.is_on() {
        MIN_MAX_TOKENS_THINKING
    } else {
        MIN_MAX_TOKENS
    };
    requested
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_MAX_TOKENS)
        .max(floor)
}

/// The system prompt as a cacheable block.
///
/// The breakpoint sits on the last system block, which caches tools *and*
/// system together (render order is tools, system, messages). afi's `SYSTEM` is
/// a frozen `const`, so the prefix is byte-stable across turns by construction -
/// nothing per-request may be interpolated ahead of this point.
fn cached_system(system: &str) -> Value {
    json!([{
        "type": "text",
        "text": system,
        "cache_control": {"type": "ephemeral"},
    }])
}

/// Copy allowlisted keys from a source's `extra_body` over the defaults, so
/// explicit configuration wins.
fn apply_allowed_extras(body: &mut Value, extra_body: Option<&Value>) {
    let Some(configured) = extra_body.and_then(Value::as_object) else {
        return;
    };
    for key in ALLOWED_EXTRA_KEYS {
        if let Some(value) = configured.get(*key) {
            body[*key] = value.clone();
        }
    }
}

/// Resolve the bearer token for a source, if its mode uses one.
async fn bearer_for(
    http: &Client,
    tokens: &TokenCache,
    source: &Source,
) -> Result<Option<String>, ClientError> {
    if !source.protocol.is_bearer() {
        return Ok(None);
    }
    tokens.bearer(http, source).await.map(Some)
}

/// Build a request with auth headers applied, then the source's own headers so
/// user-configured values still win (matching the `OpenAI` path).
async fn authed_post(
    http: &Client,
    tokens: &TokenCache,
    source: &Source,
    url: String,
    body: &Value,
) -> Result<reqwest::RequestBuilder, ClientError> {
    let bearer = bearer_for(http, tokens, source).await?;
    let headers = auth::auth_headers(&source.protocol, &source.api_key, bearer.as_deref())?;
    let mut request = http.post(url).headers(headers).json(body);
    if let Some(extra) = ReqwestClient::build_headers(source) {
        request = request.headers(extra);
    }
    Ok(request)
}

/// `POST /v1/messages` with `stream: true`, returning the parsed chunk stream.
pub(super) async fn stream(
    http: &Client,
    tokens: &TokenCache,
    request: StreamRequest<'_>,
) -> Result<ChatCompletionStream, ClientError> {
    let source = request.source;
    let body = build_body(&BodyParams {
        model: request.model,
        history: request.messages,
        tools: request.tools,
        tool_choice: request.tool_choice,
        max_tokens: request.max_tokens,
        extra_body: request.extra_body,
        stream: true,
    });
    let response = authed_post(http, tokens, source, messages_url(&source.base_url), &body)
        .await?
        .send()
        .await
        .map_err(|e| transport_error(&e))?;
    if !response.status().is_success() {
        let status = response.status().as_u16();
        return Err(ClientError::Http {
            status,
            body: limited_error_body(response).await,
        });
    }
    let bytes = response.bytes_stream().map_err(io::Error::other);
    Ok(decoded_stream(
        StreamReader::new(bytes),
        Box::new(AnthropicDecoder::new()),
    ))
}

/// `POST /v1/messages` without streaming, used by `/compress`.
///
/// The response is reshaped into `OpenAI` form so the REPL's existing
/// `choices[0].message.content` parsing needs no protocol branch.
pub(super) async fn complete(
    http: &Client,
    tokens: &TokenCache,
    source: &Source,
    model: &str,
    history: &[Value],
    timeout: u64,
    extra_body: Option<&Value>,
) -> Result<String, ClientError> {
    let body = build_body(&BodyParams {
        model,
        history,
        tools: None,
        tool_choice: None,
        max_tokens: None,
        extra_body,
        stream: false,
    });
    let response = authed_post(http, tokens, source, messages_url(&source.base_url), &body)
        .await?
        // Non-streaming, so a total deadline is meaningful here. The streaming
        // path deliberately has none.
        .timeout(Duration::from_secs(timeout))
        .send()
        .await
        .map_err(|e| transport_error(&e))?;
    let status = response.status();
    let text = response.text().await.map_err(|e| transport_error(&e))?;
    if !status.is_success() {
        return Err(ClientError::Http {
            status: status.as_u16(),
            body: text,
        });
    }
    record_completion_usage(model, &text);
    reshape_completion(&text)
}

/// Fold a non-streaming response's usage into the run totals.
///
/// The streaming path records through `finalize_turn`, which this never reaches,
/// so without this a `/compress` request is billed but absent from the summary.
/// Best effort: a response with no usage object is simply not counted.
fn record_completion_usage(model: &str, body: &str) {
    if let Some(normalized) = completion_usage(body) {
        usage_totals::record(model, &normalized);
    }
}

/// Normalize a non-streaming response's `usage` object, or `None` when it has
/// none. Split from the recording so the accounting is testable without the
/// process-wide accumulator.
fn completion_usage(body: &str) -> Option<NormalizedUsage> {
    let value = serde_json::from_str::<Value>(body).ok()?;
    let usage = value.get("usage")?;
    let field = |name: &str| usage.get(name).and_then(Value::as_u64).unwrap_or_default();
    let cache_read = field("cache_read_input_tokens");
    let cache_write = field("cache_creation_input_tokens");
    // Same re-inflation the SSE decoder applies: Anthropic's `input_tokens`
    // already excludes both cache counts, so `prompt_tokens` is the whole
    // context and the two are reported as the subsets of it that they are.
    let openai_shaped = Usage {
        prompt_tokens: field("input_tokens")
            .saturating_add(cache_read)
            .saturating_add(cache_write),
        completion_tokens: field("output_tokens"),
        prompt_tokens_details: Some(PromptTokensDetails {
            cached_tokens: cache_read,
            cache_write_tokens: cache_write,
        }),
        output_tokens_details: None,
    };
    normalize_usage(Some(&openai_shaped), None, 0)
}

/// Anthropic `{"content":[{"type":"text","text":...}]}` ->
/// `OpenAI` `{"choices":[{"message":{"content":"..."}}]}`.
///
/// Fails rather than returning an empty success. The only caller feeds this
/// straight into `apply_compression`, which replaces the conversation with the
/// summary and has no empty-summary guard, so a text-free response must be
/// distinguishable from a successful empty summary. A 200 can legitimately carry
/// no text: a pre-output refusal returns an empty `content` array, and a
/// `max_tokens` stop can leave only a thinking block.
fn reshape_completion(body: &str) -> Result<String, ClientError> {
    let value: Value = serde_json::from_str(body)
        .map_err(|e| ClientError::Parse(format!("Anthropic response was not JSON: {e}")))?;
    let text = join_text_blocks(value.get("content")).ok_or_else(|| {
        ClientError::Parse(format!(
            "Anthropic response had no content array{}",
            stop_reason_hint(&value)
        ))
    })?;
    if text.trim().is_empty() {
        return Err(ClientError::Parse(format!(
            "Anthropic returned no text{}",
            stop_reason_hint(&value)
        )));
    }
    Ok(json!({"choices": [{"message": {"content": text}}]}).to_string())
}

/// Name the stop reason when the response carries one - it is usually why there
/// was no text (`refusal`, `max_tokens`).
fn stop_reason_hint(value: &Value) -> String {
    match value.get("stop_reason").and_then(Value::as_str) {
        Some(reason) => format!(" (stop_reason: {reason})"),
        None => String::new(),
    }
}

fn join_text_blocks(content: Option<&Value>) -> Option<String> {
    let blocks = content?.as_array()?;
    Some(
        blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
    )
}

#[cfg(test)]
mod tests;
