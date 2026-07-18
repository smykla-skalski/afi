//! HTTP client for OpenAI-compatible endpoints. Uses reqwest (async) for
//! all API calls: chat completions (streaming + non-streaming), /v1/models,
//! /props, and the over-max_tokens context probe.
//!
//! A `ChatClient` trait abstracts the interface so tests can mock it without
//! a live server.

use async_trait::async_trait;
use serde_json::Value;

use crate::config::Source;
use crate::model::stream::StreamChunk;

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
    /// POST /v1/chat/completions with streaming. Returns a Vec of SSE chunks.
    async fn chat_completions_stream(
        &self,
        req: StreamRequest<'_>,
    ) -> Result<Vec<StreamChunk>, ClientError>;

    /// POST /v1/chat/completions without streaming. Returns the response text.
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

    /// POST /v1/chat/completions with a deliberately over-large max_tokens
    /// to trigger the "maximum context length is N tokens" error.
    async fn overrun_probe(&self, source: &Source, model: &str) -> Result<String, ClientError>;
}

/// Client errors: connection failures, HTTP errors, parse errors.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("connection error: {0}")]
    Connection(String),
    #[error("HTTP {status}: {body}")]
    Http { status: u16, body: String },
    #[error("parse error: {0}")]
    Parse(String),
}

/// A reqwest-based implementation of `ChatClient`.
pub struct ReqwestClient {
    client: reqwest::Client,
}

impl Default for ReqwestClient {
    fn default() -> Self {
        Self::new()
    }
}

impl ReqwestClient {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .build()
                .expect("failed to build reqwest client"),
        }
    }

    fn build_headers(source: &Source) -> Option<reqwest::header::HeaderMap> {
        source.http_headers.as_ref().map(|h| {
            let mut map = reqwest::header::HeaderMap::new();
            for (k, v) in h {
                if let (Ok(name), Ok(val)) = (
                    reqwest::header::HeaderName::try_from(k),
                    reqwest::header::HeaderValue::try_from(v),
                ) {
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
        let re = regex::Regex::new(r"/v\d+/?$").unwrap();
        let root = re.replace(root, "");
        format!("{}/props", root.trim_end_matches('/'))
    }
}

#[async_trait]
impl ChatClient for ReqwestClient {
    async fn chat_completions_stream(
        &self,
        stream_req: StreamRequest<'_>,
    ) -> Result<Vec<StreamChunk>, ClientError> {
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
        if let Some(mt) = max_tokens {
            if mt > 0 {
                body["max_tokens"] = serde_json::Value::from(mt);
            }
        }
        if let Some(eb) = extra_body {
            if let (Some(a), Some(b)) = (eb.as_object(), body.as_object_mut()) {
                for (k, v) in a {
                    b.insert(k.clone(), v.clone());
                }
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
            let body = resp.text().await.unwrap_or_default();
            return Err(ClientError::Http { status, body });
        }
        let text = resp
            .text()
            .await
            .map_err(|e| ClientError::Connection(e.to_string()))?;
        Ok(crate::model::stream::parse_sse_body(&text))
    }

    async fn chat_completions(
        &self,
        source: &Source,
        model: &str,
        messages: &[Value],
        timeout: u64,
        extra_body: Option<&Value>,
    ) -> Result<String, ClientError> {
        let mut body = serde_json::json!({
            "model": model,
            "messages": messages,
            "stream": false,
        });
        if let Some(eb) = extra_body {
            if let (Some(a), Some(b)) = (eb.as_object(), body.as_object_mut()) {
                for (k, v) in a {
                    b.insert(k.clone(), v.clone());
                }
            }
        }
        let mut req = self
            .client
            .post(Self::chat_url(source))
            .header("Authorization", format!("Bearer {}", source.api_key))
            .timeout(std::time::Duration::from_secs(timeout))
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
        resp.text()
            .await
            .map_err(|e| ClientError::Connection(e.to_string()))
    }

    async fn list_models(&self, source: &Source) -> Result<Value, ClientError> {
        let mut req = self
            .client
            .get(Self::models_url(source))
            .header("Authorization", format!("Bearer {}", source.api_key))
            .timeout(std::time::Duration::from_secs(10));
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
        let resp = self
            .client
            .get(Self::props_url(source))
            .timeout(std::time::Duration::from_secs(5))
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
            .timeout(std::time::Duration::from_secs(30))
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
