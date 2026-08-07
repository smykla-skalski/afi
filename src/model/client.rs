//! The model client: the `ChatClient` interface, its reqwest implementation,
//! and the protocol dispatch between them.
//!
//! Two wire protocols live in submodules - [`openai`] for `/chat/completions`
//! and [`anthropic`] for the Messages API. Which one serves a request is
//! resolved per call from `Source::protocol`, because `/source` can switch
//! endpoints mid-session. Amazon Bedrock is the first, with an AWS `SigV4`
//! signature in place of a credential header; [`bedrock`] holds only that
//! difference.
//!
//! A `ChatClient` trait abstracts the interface so tests can mock it without
//! a live server.

use async_trait::async_trait;
use futures::{Stream, StreamExt};
use reqwest::Client;

use serde_json::Value;

use crate::config::Source;
use crate::model::stream::StreamChunk;
use crate::summary::ErrorKind;
use reqwest::header::HeaderMap;
use reqwest::header::HeaderName;
use reqwest::header::HeaderValue;
use std::pin::Pin;

/// Live chunks from one streaming chat-completions response.
pub type ChatCompletionStream =
    Pin<Box<dyn Stream<Item = Result<StreamChunk, ClientError>> + Send>>;

mod anthropic;
mod bedrock;
mod openai;
mod redact;
mod sse;
use anthropic::TokenCache;
use anthropic::thinking::strip_history;
use redact::{Credential, Redactor};

pub(crate) use anthropic::thinking::{THINKING_HISTORY_KEY, thinking_disabled};

const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;

/// How much of a failing response body to quote at a human: enough to name the
/// cause, not enough to bury it.
///
/// One policy, so the sentence a rejected credential prints and the one an HTTP
/// error prints are cut to the same length. Two spellings of the number would be
/// kept equal only by whoever remembered both.
pub(crate) const BODY_PREVIEW_CHARS: usize = 200;

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

/// Client errors: transport failures, HTTP errors, unusable responses, and the
/// credential problems caught before any request goes out.
///
/// The variants are cut where the [`ErrorKind`] boundaries are, so a failed run
/// classifies itself from the error it already has. Anything coarser would leave
/// the summary substring-matching its own message.
///
/// Every body reaching one of these has already been through [`Redactor`], so no
/// caller has to strip a credential from it again. That is the point of doing it
/// here: the same body goes to stderr, to the summary, and to the summary file.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("connection error: {0}")]
    Connection(String),
    /// The request outlived its deadline. Apart from [`Self::Connection`] because
    /// a deadline says the server was reachable and slow, not absent.
    #[error("timed out: {0}")]
    Timeout(String),
    #[error("HTTP {status}: {body}")]
    Http { status: u16, body: String },
    /// The response body broke off part-way. Apart from [`Self::Connection`] for
    /// the same reason: a turn was already in flight.
    #[error("stream error: {0}")]
    Stream(String),
    #[error("parse error: {0}")]
    Parse(String),
    /// The credential is missing, unusable, or was refused. Kept separate so the
    /// REPL does not blame an unreachable server for what is actually a missing
    /// credential, and so a caller never retries one.
    #[error("{0}")]
    Auth(String),
    /// afi called itself wrongly. Not reachable from any configuration, so it is
    /// a bug rather than something a caller can fix.
    #[error("{0}")]
    Internal(String),
}

impl ClientError {
    /// Which closed-set kind this failure is, for the run summary.
    #[must_use]
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::Connection(_) => ErrorKind::ProviderHttp,
            Self::Timeout(_) => ErrorKind::Timeout,
            Self::Http { status, .. } => http_kind(*status),
            Self::Stream(_) | Self::Parse(_) => ErrorKind::ProviderStream,
            Self::Auth(_) => ErrorKind::Auth,
            Self::Internal(_) => ErrorKind::Internal,
        }
    }
}

/// A failing status by what a caller should do about it: 401 and 403 are the
/// credential and will say the same thing next time, 408 and 504 are deadlines,
/// and everything left - a 429 included - is the provider's own trouble.
fn http_kind(status: u16) -> ErrorKind {
    match status {
        401 | 403 => ErrorKind::Auth,
        408 | 504 => ErrorKind::Timeout,
        _ => ErrorKind::ProviderHttp,
    }
}

/// Classify a reqwest transport failure: a request that outlived its deadline is
/// worth another attempt on a longer one, and one that never landed is the
/// server or the network between.
fn transport_error(error: &reqwest::Error) -> ClientError {
    transport_error_at(None, error)
}

/// The same, naming the step that failed.
///
/// The identity-exchange calls need it - "connection refused" alone does not say
/// which of the two endpoints refused, and they are operated by different people.
/// The sentence is built from the error being classified rather than passed in
/// beside it, so the two can never describe different failures.
fn transport_error_at(what: Option<&str>, error: &reqwest::Error) -> ClientError {
    let message = match what {
        Some(what) => format!("{what}: {error}"),
        None => error.to_string(),
    };
    if error.is_timeout() {
        ClientError::Timeout(message)
    } else {
        ClientError::Connection(message)
    }
}

/// The `OpenAI`-only probe endpoints have no Anthropic equivalent. None of them
/// has a production caller today, so a clear error beats a half implementation.
fn unsupported(source: &Source, what: &str) -> ClientError {
    ClientError::Internal(format!(
        "{what} is not available on the Anthropic protocol (source {})",
        source.name
    ))
}

/// Turn credential-bearing name/value pairs into a `HeaderMap`.
///
/// Shared by both protocols that build their auth headers by hand - Anthropic's
/// `x-api-key`/bearer set and Bedrock's `SigV4` set - so the rule that a
/// rejected value is never echoed holds in one place rather than two.
fn into_header_map(pairs: Vec<(&str, String)>) -> Result<HeaderMap, ClientError> {
    let mut map = HeaderMap::new();
    for (name, value) in pairs {
        // Neither failure is a wire-parse problem, so neither is reported as one.
        // The names are afi's own literals, so a rejected one is a bug; the
        // values are credentials, and one copied with a trailing newline lands
        // here. Left to reqwest, both would surface at `send` and be blamed on an
        // unreachable server.
        let header = HeaderName::try_from(name)
            .map_err(|e| ClientError::Internal(format!("bad header name {name}: {e}")))?;
        // The error deliberately omits the value: it may be a credential.
        let header_value = HeaderValue::try_from(value)
            .map_err(|_| ClientError::Auth(format!("invalid characters in {name} value")))?;
        map.insert(header, header_value);
    }
    Ok(map)
}

/// Read a failing response's body, bounded, with the credentials the request
/// carried struck out of it.
async fn limited_error_body(response: reqwest::Response, redact: &Redactor) -> String {
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
            return truncated_body(&body, redact);
        }
    }
    reported_body(&body, redact)
}

/// What a failing response reports: its body, with the credentials the request
/// carried struck out.
fn reported_body(body: &[u8], redact: &Redactor) -> String {
    redact.clean(&String::from_utf8_lossy(body))
}

/// The same, for a body [`MAX_ERROR_BODY_BYTES`] cut short.
///
/// Cleaning runs before the marker goes on rather than after. That cap is the
/// one limit landing before redaction can look at the whole body, so a
/// credential straddling it leaves its opening behind - which
/// [`Redactor::clean`] strips as a severed tail. Marking first would hide that
/// opening behind `[truncated]`, where nothing would go looking for it.
fn truncated_body(body: &[u8], redact: &Redactor) -> String {
    format!("{}\n[truncated]", reported_body(body, redact))
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
        openai::stream(&self.client, stream_req).await
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
        openai::complete(&self.client, source, model, messages, timeout, extra_body).await
    }

    async fn list_models(&self, source: &Source) -> Result<Value, ClientError> {
        if source.is_anthropic() {
            return Err(unsupported(source, "model listing"));
        }
        openai::list_models(&self.client, source).await
    }

    async fn get_props(&self, source: &Source) -> Result<Value, ClientError> {
        if source.is_anthropic() {
            return Err(unsupported(source, "the llama.cpp /props probe"));
        }
        openai::get_props(&self.client, source).await
    }

    async fn overrun_probe(&self, source: &Source, model: &str) -> Result<String, ClientError> {
        if source.is_anthropic() {
            return Err(unsupported(source, "the context-overrun probe"));
        }
        openai::overrun_probe(&self.client, source, model).await
    }
}

#[cfg(test)]
mod tests;
