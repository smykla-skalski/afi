//! The `OpenAI`-compatible protocol: `/chat/completions` streaming and
//! non-streaming, `/v1/models`, llama.cpp's `/props`, and the context-overrun
//! probe.
//!
//! The sibling of [`super::anthropic`]. [`super::ReqwestClient`] picks between
//! the two per request, because `/source` can switch endpoints mid-session.
//!
//! Unlike the Anthropic path, request bodies are close to what the caller
//! passed: `extra_body` merges at the top level unfiltered, because every
//! backend here (llama.cpp, vLLM, `SGLang`, Z.ai, `OpenAI`, `OpenRouter`) has its
//! own extensions and afi has no list of them. The one thing that is removed is
//! afi's own thinking-block key, which is not part of any wire format.

use futures::TryStreamExt;
use regex::Regex;
use reqwest::Client;
use serde_json::Value;
use tokio_util::io::StreamReader;

use std::io;
use std::time::Duration;

use super::sse::{OpenAiDecoder, decoded_stream};
use super::{
    ChatCompletionStream, ClientError, ReqwestClient, StreamRequest, limited_error_body,
    strip_history, transport_error,
};
use crate::config::Source;
use crate::model::stream::{Usage, normalize_usage};
use crate::model::usage_totals;

fn chat_url(source: &Source) -> String {
    format!("{}/chat/completions", source.base_url.trim_end_matches('/'))
}

fn models_url(source: &Source) -> String {
    format!("{}/models", source.base_url.trim_end_matches('/'))
}

fn props_url(source: &Source) -> String {
    // /props is at the root, not under /v1
    let root = source.base_url.trim_end_matches('/');
    let re = Regex::new(r"/v\d+/?$").unwrap();
    let root = re.replace(root, "");
    format!("{}/props", root.trim_end_matches('/'))
}

/// Merge a source's `extra_body` over the request body at the top level.
fn merge_extra_body(body: &mut Value, extra_body: Option<&Value>) {
    if let Some(eb) = extra_body
        && let (Some(extra), Some(target)) = (eb.as_object(), body.as_object_mut())
    {
        for (key, value) in extra {
            target.insert(key.clone(), value.clone());
        }
    }
}

/// Build a POST with the bearer credential, then the source's own headers so
/// user-configured values still win.
fn authed_post(
    http: &Client,
    source: &Source,
    url: String,
    body: &Value,
) -> reqwest::RequestBuilder {
    let mut request = http
        .post(url)
        .header("Authorization", format!("Bearer {}", source.api_key))
        .json(body);
    if let Some(headers) = ReqwestClient::build_headers(source) {
        request = request.headers(headers);
    }
    request
}

/// The streaming request body.
///
/// Split from [`stream`] so the shape can be asserted without a live endpoint -
/// notably that `strip_history` ran, which nothing downstream would notice
/// until a real `OpenAI` request came back 400.
fn stream_body(request: &StreamRequest<'_>) -> Value {
    let mut body = serde_json::json!({
        "model": request.model,
        // Anthropic thinking blocks are afi's own key on the message, and an
        // endpoint here may reject an unrecognized message field outright.
        "messages": strip_history(request.messages).as_ref(),
        "stream": true,
        "stream_options": {"include_usage": true},
    });
    if let Some(tools) = request.tools {
        body["tools"] = tools.clone();
    }
    if let Some(choice) = request.tool_choice {
        body["tool_choice"] = choice.clone();
    }
    if let Some(limit) = request.max_tokens.filter(|value| *value > 0) {
        body[max_tokens_key(request.source)] = Value::from(limit);
    }
    merge_extra_body(&mut body, request.extra_body);
    body
}

/// The output-limit parameter this endpoint takes.
///
/// `max_tokens` is the spelling every `OpenAI`-compatible server implements,
/// and the one `OpenAI`'s own reasoning models refuse outright: they take
/// `max_completion_tokens` and 400 on the older key. Those are exactly the
/// models `reasoning_effort` applies to, so without this the effort dialect afi
/// advertises for that host could never produce a request it would accept.
fn max_tokens_key(source: &Source) -> &'static str {
    if source.is_openai() {
        "max_completion_tokens"
    } else {
        "max_tokens"
    }
}

/// The non-streaming request body, used by `/compress`.
fn completion_body(model: &str, messages: &[Value], extra_body: Option<&Value>) -> Value {
    let mut body = serde_json::json!({
        "model": model,
        "messages": strip_history(messages).as_ref(),
        "stream": false,
    });
    merge_extra_body(&mut body, extra_body);
    body
}

/// `POST /chat/completions` with `stream: true`, returning the parsed chunk
/// stream.
pub(super) async fn stream(
    http: &Client,
    request: StreamRequest<'_>,
) -> Result<ChatCompletionStream, ClientError> {
    let source = request.source;
    let body = stream_body(&request);
    let response = authed_post(http, source, chat_url(source), &body)
        .send()
        .await
        .map_err(|e| transport_error(&e))?;
    if !response.status().is_success() {
        let status = response.status().as_u16();
        let body = limited_error_body(response).await;
        return Err(ClientError::Http { status, body });
    }
    let bytes = response.bytes_stream().map_err(io::Error::other);
    Ok(decoded_stream(
        StreamReader::new(bytes),
        Box::new(OpenAiDecoder),
    ))
}

/// `POST /chat/completions` without streaming, used by `/compress`.
pub(super) async fn complete(
    http: &Client,
    source: &Source,
    model: &str,
    messages: &[Value],
    timeout: u64,
    extra_body: Option<&Value>,
) -> Result<String, ClientError> {
    let body = completion_body(model, messages, extra_body);
    let response = authed_post(http, source, chat_url(source), &body)
        .timeout(Duration::from_secs(timeout))
        .send()
        .await
        .map_err(|e| transport_error(&e))?;
    if !response.status().is_success() {
        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();
        return Err(ClientError::Http { status, body });
    }
    let text = response.text().await.map_err(|e| transport_error(&e))?;
    record_completion_usage(&source.name, model, &text);
    Ok(text)
}

/// Fold a non-streaming response's usage into the run totals.
///
/// The streaming path records through `finalize_turn`, which this never reaches,
/// so without this a `/compress` request is billed but missing from the run
/// summary. Best effort: a body with no usage object is simply not counted.
fn record_completion_usage(source: &str, model: &str, body: &str) {
    let Ok(parsed) = serde_json::from_str::<CompletionUsage>(body) else {
        return;
    };
    let Some(usage) = parsed.usage else {
        return;
    };
    if let Some(normalized) = normalize_usage(Some(&usage), None, 0) {
        usage_totals::record(source, model, &normalized);
    }
}

/// Just the usage object, so an unknown response shape still parses.
#[derive(serde::Deserialize)]
struct CompletionUsage {
    usage: Option<Usage>,
}

/// `GET /models`.
pub(super) async fn list_models(http: &Client, source: &Source) -> Result<Value, ClientError> {
    let mut request = http
        .get(models_url(source))
        .header("Authorization", format!("Bearer {}", source.api_key))
        .timeout(Duration::from_secs(10));
    if let Some(headers) = ReqwestClient::build_headers(source) {
        request = request.headers(headers);
    }
    json_response(request).await
}

/// `GET /props`, llama.cpp's server-configuration endpoint.
pub(super) async fn get_props(http: &Client, source: &Source) -> Result<Value, ClientError> {
    json_response(http.get(props_url(source)).timeout(Duration::from_secs(5))).await
}

async fn json_response(request: reqwest::RequestBuilder) -> Result<Value, ClientError> {
    let response = request.send().await.map_err(|e| transport_error(&e))?;
    if !response.status().is_success() {
        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();
        return Err(ClientError::Http { status, body });
    }
    response
        .json()
        .await
        .map_err(|e| ClientError::Parse(e.to_string()))
}

/// `POST /chat/completions` with a deliberately over-large `max_tokens`, to
/// make the server name its context limit in the error.
pub(super) async fn overrun_probe(
    http: &Client,
    source: &Source,
    model: &str,
) -> Result<String, ClientError> {
    let mut body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": "hi"}],
        "stream": false,
    });
    // Same spelling the real request uses, or the probe reads back a complaint
    // about the parameter instead of the limit it went looking for.
    body[max_tokens_key(source)] = Value::from(10_000_000);
    let response = authed_post(http, source, chat_url(source), &body)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| transport_error(&e))?;
    // The probe expects a 400 with the context limit in the error, so the body
    // is returned either way for the caller to parse.
    Ok(response.text().await.unwrap_or_default())
}

#[cfg(test)]
mod tests;
