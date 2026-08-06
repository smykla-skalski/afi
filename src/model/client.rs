//! HTTP client for OpenAI-compatible endpoints. Uses reqwest (async) for
//! all API calls: chat completions (streaming + non-streaming), /v1/models,
//! /props, and the over-max_tokens context probe.
//!
//! A `ChatClient` trait abstracts the interface so tests can mock it without
//! a live server.

use async_trait::async_trait;
use futures::{Stream, StreamExt, TryStreamExt};
use regex::Regex;
use reqwest::Client;
use tokio_util::io::StreamReader;

use serde_json::Value;

use crate::config::Source;
use crate::model::stream::{StreamChunk, Usage, normalize_usage};
use crate::model::usage_totals;
use reqwest::header::HeaderMap;
use reqwest::header::HeaderName;
use reqwest::header::HeaderValue;
use std::io;
use std::pin::Pin;
use std::time::Duration;

/// Live chunks from one streaming chat-completions response.
pub type ChatCompletionStream =
    Pin<Box<dyn Stream<Item = Result<StreamChunk, ClientError>> + Send>>;

mod anthropic;
mod sse;
use anthropic::TokenCache;
use sse::{OpenAiDecoder, decoded_stream};

const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;

/// Bundles the parameters for a streaming chat completion request.
#[derive(Debug, Clone)]
pub struct StreamRequest<'a> {
    pub source: &'a Source,
    pub model: &'a str,
    pub messages: &'a [Value],
    pub tools: Option<&'a Value>,
    pub tool_choice: Option<&'a Value>,
    pub max_tokens: Option<u32>,
    pub extra_body: Option<&'a Value>,
    pub recovery_sampling: bool,
}

/// Trait for the model client's API calls. The real implementation uses
/// reqwest; tests can mock it.
#[async_trait]
pub trait ChatClient: Send + Sync {
    /// POST /v1/chat/completions and return its live parsed SSE stream.
    async fn chat_completions_stream(
        &self,
        req: StreamRequest<'_>,
    ) -> Result<ChatCompletionStream, ClientError>;

    /// POST /v1/chat/completions without streaming. Returns the response text.
    ///
    /// `extra_body` is the source's own body keys, **unwrapped** - they are
    /// merged at the top level of the request. Passing `{"extra_body": {...}}`
    /// would send a literal `extra_body` key, which every backend ignores.
    async fn chat_completions(
        &self,
        source: &Source,
        model: &str,
        messages: &[Value],
        timeout: u64,
        extra_body: Option<&Value>,
    ) -> Result<String, ClientError>;

    /// GET /v1/models. Returns the parsed JSON.
    async fn list_models(&self, source: &Source) -> Result<Value, ClientError>;

    /// GET /props (llama.cpp). Returns the parsed JSON.
    async fn get_props(&self, source: &Source) -> Result<Value, ClientError>;

    /// POST /v1/chat/completions with a deliberately over-large `max_tokens`
    /// to trigger the "maximum context length is N tokens" error.
    async fn overrun_probe(&self, source: &Source, model: &str) -> Result<String, ClientError>;
}

/// Client errors: connection failures, HTTP errors, parse errors, and local
/// misconfiguration caught before any request goes out.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("connection error: {0}")]
    Connection(String),
    #[error("HTTP {status}: {body}")]
    Http { status: u16, body: String },
    #[error("parse error: {0}")]
    Parse(String),
    /// Nothing was sent because the source is not usable as configured. Kept
    /// separate so the REPL does not blame an unreachable server for what is
    /// actually a missing credential.
    #[error("{0}")]
    Config(String),
}

/// The `OpenAI`-only probe endpoints have no Anthropic equivalent. None of them
/// has a production caller today, so a clear error beats a half implementation.
/// Fold a non-streaming response's usage into the run totals.
///
/// The streaming path records through `finalize_turn`, which this never reaches,
/// so without this a `/compress` request is billed but missing from the run
/// summary. Best effort: a body with no usage object is simply not counted.
fn record_completion_usage(body: &str) {
    let Ok(parsed) = serde_json::from_str::<CompletionUsage>(body) else {
        return;
    };
    let Some(usage) = parsed.usage else {
        return;
    };
    if let Some(normalized) = normalize_usage(Some(&usage), None, 0) {
        usage_totals::record(&normalized);
    }
}

/// Just the usage object, so an unknown response shape still parses.
#[derive(serde::Deserialize)]
struct CompletionUsage {
    usage: Option<Usage>,
}

fn unsupported(source: &Source, what: &str) -> ClientError {
    ClientError::Config(format!(
        "{what} is not available on the Anthropic protocol (source {})",
        source.name
    ))
}

async fn limited_error_body(response: reqwest::Response) -> String {
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else {
            break;
        };
        let remaining = (MAX_ERROR_BODY_BYTES + 1).saturating_sub(body.len());
        body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        if body.len() > MAX_ERROR_BODY_BYTES {
            body.truncate(MAX_ERROR_BODY_BYTES);
            return format!("{}\n[truncated]", String::from_utf8_lossy(&body));
        }
    }
    String::from_utf8_lossy(&body).into_owned()
}

/// A reqwest-based implementation of `ChatClient`.
///
/// Speaks both wire protocols. Which one a request uses is resolved per call
/// from `Source::protocol`, because `/source` can switch endpoints mid-session.
pub struct ReqwestClient {
    client: reqwest::Client,
    /// Federated Anthropic access tokens, cached until they near expiry.
    tokens: TokenCache,
}

impl Default for ReqwestClient {
    fn default() -> Self {
        Self::new()
    }
}

impl ReqwestClient {
    /// Build a client with the default TLS backend.
    ///
    /// # Panics
    /// Panics if the underlying reqwest client cannot be built (e.g. the TLS
    /// backend fails to initialize).
    #[must_use]
    pub fn new() -> Self {
        Self {
            client: Client::builder()
                .build()
                .expect("failed to build reqwest client"),
            tokens: TokenCache::default(),
        }
    }

    fn build_headers(source: &Source) -> Option<HeaderMap> {
        source.http_headers.as_ref().map(|h| {
            let mut map = HeaderMap::new();
            for (k, v) in h {
                if let (Ok(name), Ok(val)) = (HeaderName::try_from(k), HeaderValue::try_from(v)) {
                    map.insert(name, val);
                }
            }
            map
        })
    }

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
}

#[async_trait]
impl ChatClient for ReqwestClient {
    async fn chat_completions_stream(
        &self,
        stream_req: StreamRequest<'_>,
    ) -> Result<ChatCompletionStream, ClientError> {
        if stream_req.source.is_anthropic() {
            return anthropic::stream(&self.client, &self.tokens, stream_req).await;
        }
        let StreamRequest {
            source,
            model,
            messages,
            tools,
            tool_choice,
            max_tokens,
            extra_body,
            recovery_sampling: _,
        } = stream_req;

        let mut body = serde_json::json!({
            "model": model,
            "messages": messages,
            "stream": true,
            "stream_options": {"include_usage": true},
        });
        if let Some(t) = tools {
            body["tools"] = t.clone();
        }
        if let Some(tc) = tool_choice {
            body["tool_choice"] = tc.clone();
        }
        if let Some(mt) = max_tokens
            && mt > 0
        {
            body["max_tokens"] = Value::from(mt);
        }
        if let Some(eb) = extra_body
            && let (Some(a), Some(b)) = (eb.as_object(), body.as_object_mut())
        {
            for (k, v) in a {
                b.insert(k.clone(), v.clone());
            }
        }

        let mut req = self
            .client
            .post(Self::chat_url(source))
            .header("Authorization", format!("Bearer {}", source.api_key))
            .json(&body);
        if let Some(h) = Self::build_headers(source) {
            req = req.headers(h);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| ClientError::Connection(e.to_string()))?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = limited_error_body(resp).await;
            return Err(ClientError::Http { status, body });
        }
        let bytes = resp.bytes_stream().map_err(io::Error::other);
        Ok(decoded_stream(
            StreamReader::new(bytes),
            Box::new(OpenAiDecoder),
        ))
    }

    async fn chat_completions(
        &self,
        source: &Source,
        model: &str,
        messages: &[Value],
        timeout: u64,
        extra_body: Option<&Value>,
    ) -> Result<String, ClientError> {
        if source.is_anthropic() {
            return anthropic::complete(
                &self.client,
                &self.tokens,
                source,
                model,
                messages,
                timeout,
                extra_body,
            )
            .await;
        }
        let mut body = serde_json::json!({
            "model": model,
            "messages": messages,
            "stream": false,
        });
        if let Some(eb) = extra_body
            && let (Some(a), Some(b)) = (eb.as_object(), body.as_object_mut())
        {
            for (k, v) in a {
                b.insert(k.clone(), v.clone());
            }
        }
        let mut req = self
            .client
            .post(Self::chat_url(source))
            .header("Authorization", format!("Bearer {}", source.api_key))
            .timeout(Duration::from_secs(timeout))
            .json(&body);
        if let Some(h) = Self::build_headers(source) {
            req = req.headers(h);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| ClientError::Connection(e.to_string()))?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ClientError::Http { status, body });
        }
        let text = resp
            .text()
            .await
            .map_err(|e| ClientError::Connection(e.to_string()))?;
        record_completion_usage(&text);
        Ok(text)
    }

    async fn list_models(&self, source: &Source) -> Result<Value, ClientError> {
        if source.is_anthropic() {
            return Err(unsupported(source, "model listing"));
        }
        let mut req = self
            .client
            .get(Self::models_url(source))
            .header("Authorization", format!("Bearer {}", source.api_key))
            .timeout(Duration::from_secs(10));
        if let Some(h) = Self::build_headers(source) {
            req = req.headers(h);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| ClientError::Connection(e.to_string()))?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ClientError::Http { status, body });
        }
        resp.json()
            .await
            .map_err(|e| ClientError::Parse(e.to_string()))
    }

    async fn get_props(&self, source: &Source) -> Result<Value, ClientError> {
        if source.is_anthropic() {
            return Err(unsupported(source, "the llama.cpp /props probe"));
        }
        let resp = self
            .client
            .get(Self::props_url(source))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| ClientError::Connection(e.to_string()))?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ClientError::Http { status, body });
        }
        resp.json()
            .await
            .map_err(|e| ClientError::Parse(e.to_string()))
    }

    async fn overrun_probe(&self, source: &Source, model: &str) -> Result<String, ClientError> {
        if source.is_anthropic() {
            return Err(unsupported(source, "the context-overrun probe"));
        }
        let body = serde_json::json!({
            "model": model,
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 10_000_000,
            "stream": false,
        });
        let mut req = self
            .client
            .post(Self::chat_url(source))
            .header("Authorization", format!("Bearer {}", source.api_key))
            .timeout(Duration::from_secs(30))
            .json(&body);
        if let Some(h) = Self::build_headers(source) {
            req = req.headers(h);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| ClientError::Connection(e.to_string()))?;
        // The overrun probe expects a 400 with the context limit in the error.
        // Even on error, return the body so the caller can parse it.
        let body = resp.text().await.unwrap_or_default();
        Ok(body)
    }
}

#[cfg(test)]
mod tests;
