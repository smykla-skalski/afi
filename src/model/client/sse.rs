//! Bounded state machine for server-sent events.
//!
//! The framing layer (`data:` line assembly, size bounds, multi-field joins) is
//! protocol-neutral; the JSON shape of each event is delegated to an
//! [`SseDecoder`]. `OpenAI`-compatible endpoints and Anthropic's Messages API
//! share this machine and differ only in that decoder.

use futures::StreamExt;
use futures::stream::unfold;
use std::mem;
use tokio::io::AsyncRead;
use tokio_util::codec::{FramedRead, LinesCodec, LinesCodecError};

use super::{ChatCompletionStream, ClientError};
use crate::model::stream::{SseDecodeError, SseLine, StreamChunk, decode_sse_data};

const MAX_SSE_LINE_BYTES: usize = 8 * 1024 * 1024;
const MAX_SSE_EVENT_BYTES: usize = 8 * 1024 * 1024;
const ERROR_PREVIEW_CHARS: usize = 200;

/// Turns one joined SSE event payload into a [`SseLine`].
///
/// # Invariant
///
/// Implementations **must parse before mutating any internal state, and must
/// not mutate on [`SseDecodeError::Json`]**. [`handle_line`] speculatively
/// decodes a payload before buffering it, so any single field of a fragmented
/// event is handed to the decoder once and expected to fail parsing. A stateful
/// decoder that mutated eagerly would double-count that fragment.
pub(crate) trait SseDecoder: Send {
    fn decode(&mut self, data: &str) -> Result<SseLine, SseDecodeError>;
}

/// Decoder for `OpenAI`-compatible `chat/completions` streams. Stateless.
pub(crate) struct OpenAiDecoder;

impl SseDecoder for OpenAiDecoder {
    fn decode(&mut self, data: &str) -> Result<SseLine, SseDecodeError> {
        decode_sse_data(data)
    }
}

struct DecodeState<R> {
    lines: FramedRead<R, LinesCodec>,
    decoder: Box<dyn SseDecoder>,
    pending: String,
    saw_data: bool,
    saw_finish: bool,
    non_sse_preview: String,
    failed: bool,
}

pub(super) fn decoded_stream<R>(reader: R, decoder: Box<dyn SseDecoder>) -> ChatCompletionStream
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let state = DecodeState {
        lines: FramedRead::new(reader, LinesCodec::new_with_max_length(MAX_SSE_LINE_BYTES)),
        decoder,
        pending: String::new(),
        saw_data: false,
        saw_finish: false,
        non_sse_preview: String::new(),
        failed: false,
    };
    Box::pin(unfold(state, |mut state| async move {
        if state.failed {
            return None;
        }
        loop {
            let step = match state.lines.next().await {
                Some(Ok(line)) if line.is_empty() => decode_pending(&mut state),
                Some(Ok(line)) => handle_line(&mut state, &line),
                // The response opened, so this is a body that broke off rather
                // than a server that was never there - and a caller retries the
                // two on different terms.
                Some(Err(LinesCodecError::Io(error))) => {
                    DecodeStep::Error(ClientError::Stream(error.to_string()))
                }
                Some(Err(LinesCodecError::MaxLineLengthExceeded)) => DecodeStep::Error(
                    ClientError::Parse(format!("SSE line exceeds {MAX_SSE_LINE_BYTES} bytes")),
                ),
                None => eof_step(&mut state),
            };
            match step {
                DecodeStep::Chunk(chunk) => {
                    state.saw_finish |= chunk.finish_reason.is_some();
                    return Some((Ok(chunk), state));
                }
                DecodeStep::Error(error) => {
                    state.failed = true;
                    return Some((Err(error), state));
                }
                DecodeStep::Done => return None,
                DecodeStep::Wait => {}
            }
        }
    }))
}

enum DecodeStep {
    Chunk(StreamChunk),
    Error(ClientError),
    Done,
    Wait,
}

fn handle_line<R>(state: &mut DecodeState<R>, line: &str) -> DecodeStep {
    let value = if line == "data" {
        ""
    } else if let Some(value) = line.strip_prefix("data:") {
        value.trim_start()
    } else {
        record_non_sse(state, line);
        return DecodeStep::Wait;
    };
    state.saw_data = true;
    if state.pending.is_empty() {
        match state.decoder.decode(value) {
            Ok(SseLine::Chunk(chunk)) => return DecodeStep::Chunk(*chunk),
            Ok(SseLine::Done) => return DecodeStep::Done,
            Ok(SseLine::Ignore) => return DecodeStep::Wait,
            Err(SseDecodeError::Provider(error)) => return provider_error(&error),
            Err(SseDecodeError::Json(_)) => {}
        }
    }
    match append_field(&mut state.pending, value, MAX_SSE_EVENT_BYTES) {
        Ok(()) => DecodeStep::Wait,
        Err(error) => DecodeStep::Error(error),
    }
}

fn append_field(pending: &mut String, value: &str, limit: usize) -> Result<(), ClientError> {
    let separator = usize::from(!pending.is_empty());
    let new_len = pending
        .len()
        .saturating_add(separator)
        .saturating_add(value.len());
    if new_len > limit {
        return Err(ClientError::Parse(format!(
            "SSE event exceeds {limit} bytes"
        )));
    }
    if separator == 1 {
        pending.push('\n');
    }
    pending.push_str(value);
    Ok(())
}

fn decode_pending<R>(state: &mut DecodeState<R>) -> DecodeStep {
    if state.pending.is_empty() {
        return DecodeStep::Wait;
    }
    let data = mem::take(&mut state.pending);
    match state.decoder.decode(&data) {
        Ok(SseLine::Chunk(chunk)) => DecodeStep::Chunk(*chunk),
        Ok(SseLine::Done) => DecodeStep::Done,
        Ok(SseLine::Ignore) => DecodeStep::Wait,
        Err(SseDecodeError::Provider(error)) => provider_error(&error),
        Err(SseDecodeError::Json(error)) => {
            DecodeStep::Error(ClientError::Parse(error.to_string()))
        }
    }
}

fn provider_error(error: &str) -> DecodeStep {
    DecodeStep::Error(ClientError::Parse(format!("provider error: {error}")))
}

fn record_non_sse<R>(state: &mut DecodeState<R>, line: &str) {
    if line.starts_with(':')
        || line.starts_with("event:")
        || line.starts_with("id:")
        || line.starts_with("retry:")
        || line.is_empty()
    {
        return;
    }
    let mut remaining = ERROR_PREVIEW_CHARS.saturating_sub(state.non_sse_preview.chars().count());
    if remaining == 0 {
        return;
    }
    if !state.non_sse_preview.is_empty() {
        state.non_sse_preview.push(' ');
        remaining = remaining.saturating_sub(1);
    }
    state.non_sse_preview.extend(line.chars().take(remaining));
}

fn eof_step<R>(state: &mut DecodeState<R>) -> DecodeStep {
    let pending = decode_pending(state);
    if !matches!(pending, DecodeStep::Wait) {
        return pending;
    }
    if state.saw_finish {
        return DecodeStep::Done;
    }
    if !state.saw_data && !state.non_sse_preview.is_empty() {
        return DecodeStep::Error(ClientError::Parse(format!(
            "response was not an SSE stream: {}",
            state.non_sse_preview
        )));
    }
    DecodeStep::Error(ClientError::Parse(
        "SSE stream ended before [DONE] or finish_reason".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn total_event_size_is_bounded() {
        let mut pending = String::new();
        append_field(&mut pending, "1234", 8).unwrap();
        let error = append_field(&mut pending, "5678", 8).unwrap_err();
        assert!(error.to_string().contains("SSE event exceeds 8 bytes"));
    }
}
