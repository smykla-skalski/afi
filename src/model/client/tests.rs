use super::sse::{OpenAiDecoder, decoded_stream};
use super::*;
use futures::StreamExt;
use std::io;
use std::io::Cursor;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWriteExt, ReadBuf, duplex};

/// The `OpenAI`-compatible framing these tests cover. Keeping the decoder
/// choice in one helper means the assertions below stayed byte-identical when
/// the framing layer became protocol-neutral.
fn openai_stream<R>(reader: R) -> ChatCompletionStream
where
    R: AsyncRead + Unpin + Send + 'static,
{
    redacting_stream(reader, Redactor::default())
}

/// The same, for a stream whose request carried a credential.
fn redacting_stream<R>(reader: R, redact: Redactor) -> ChatCompletionStream
where
    R: AsyncRead + Unpin + Send + 'static,
{
    decoded_stream(reader, Box::new(OpenAiDecoder), redact)
}

#[tokio::test]
async fn decoded_stream_reassembles_fragmented_sse_lines() {
    let (mut writer, reader) = duplex(16);
    let write = tokio::spawn(async move {
        writer
            .write_all(b"data: {\"choices\":[{\"delta\":{\"content\":\"hel")
            .await
            .unwrap();
        writer.write_all(b"lo\"}}]}\ndata: [DONE]\n").await.unwrap();
    });

    let mut stream = openai_stream(reader);
    let chunk = stream.next().await.unwrap().unwrap();
    assert_eq!(chunk.content.as_deref(), Some("hello"));
    assert!(stream.next().await.is_none());
    write.await.unwrap();
}

#[tokio::test]
async fn decoded_stream_joins_multiple_data_fields() {
    let input = b"data: {\"choices\":[\ndata: {\"delta\":{\"content\":\"hello\"}}\ndata: ]}\n\ndata: [DONE]\n\n";
    let mut stream = openai_stream(&input[..]);
    let chunk = stream.next().await.unwrap().unwrap();
    assert_eq!(chunk.content.as_deref(), Some("hello"));
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn decoded_stream_rejects_non_sse_success_body() {
    let mut stream = openai_stream(&b"{\"error\":\"proxy failure\"}\n"[..]);
    let error = stream.next().await.unwrap().unwrap_err();
    assert!(error.to_string().contains("response was not an SSE stream"));
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn decoded_stream_reports_clean_eof_before_completion() {
    let input = b"data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n";
    let mut stream = openai_stream(&input[..]);
    let chunk = stream.next().await.unwrap().unwrap();
    assert_eq!(chunk.content.as_deref(), Some("partial"));
    let error = stream.next().await.unwrap().unwrap_err();
    assert!(error.to_string().contains("ended before [DONE]"));
}

#[tokio::test]
async fn decoded_stream_accepts_finish_reason_without_done_marker() {
    let input = b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n";
    let mut stream = openai_stream(&input[..]);
    let chunk = stream.next().await.unwrap().unwrap();
    assert_eq!(chunk.finish_reason.as_deref(), Some("stop"));
    assert!(stream.next().await.is_none());
}

// --- failure classification ---------------------------------------------------

#[test]
fn a_status_is_classified_by_what_a_caller_should_do_about_it() {
    // The retry decision, in one table. A 401 or 403 will say the same thing next
    // time; a 429 is capacity and a 500 is a bad minute, and both are what a
    // retry is for.
    let cases = [
        (401, ErrorKind::Auth),
        (403, ErrorKind::Auth),
        (408, ErrorKind::Timeout),
        (504, ErrorKind::Timeout),
        (400, ErrorKind::ProviderHttp),
        (429, ErrorKind::ProviderHttp),
        (500, ErrorKind::ProviderHttp),
        (503, ErrorKind::ProviderHttp),
    ];
    for (status, kind) in cases {
        let error = ClientError::Http {
            status,
            body: String::new(),
        };
        assert_eq!(error.kind(), kind, "HTTP {status}");
    }
}

#[test]
fn a_broken_stream_is_told_apart_from_a_server_that_was_never_there() {
    // Both are transport failures, and a caller reporting one wants to know which:
    // a cut body means the request was accepted and the turn was under way.
    assert_eq!(
        ClientError::Stream("connection reset".to_string()).kind(),
        ErrorKind::ProviderStream
    );
    assert_eq!(
        ClientError::Parse("no finish_reason".to_string()).kind(),
        ErrorKind::ProviderStream
    );
    assert_eq!(
        ClientError::Connection("refused".to_string()).kind(),
        ErrorKind::ProviderHttp
    );
    assert_eq!(
        ClientError::Timeout("elapsed".to_string()).kind(),
        ErrorKind::Timeout
    );
}

/// A body that hands over one event and then drops, the way a connection cut
/// mid-answer reaches the decoder. `duplex` cannot express this: closing its
/// writer is a clean EOF, which is a different failure.
struct CutBody {
    head: &'static [u8],
}

impl AsyncRead for CutBody {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.head.is_empty() {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "connection reset by peer",
            )));
        }
        let taken = this.head.len().min(buf.remaining());
        buf.put_slice(&this.head[..taken]);
        this.head = &this.head[taken..];
        Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn a_body_that_dies_mid_stream_reports_a_stream_failure() {
    // The classification has to come off the real path: `Connection` here would
    // report the provider as unreachable when it had already answered.
    let mut stream = openai_stream(CutBody {
        head: b"data: {\"choices\":[{\"delta\":{\"content\":\"half\"}}]}\n",
    });
    let chunk = stream.next().await.unwrap().unwrap();
    assert_eq!(chunk.content.as_deref(), Some("half"));
    let error = stream.next().await.unwrap().unwrap_err();
    assert!(matches!(error, ClientError::Stream(_)), "got {error:?}");
    assert_eq!(error.kind(), ErrorKind::ProviderStream);
}

// --- what a bounded error body reports ----------------------------------------

/// Stands in for a bearer token echoed back inside a refusal.
const TOKEN: &str = "sk-ant-oat01-echoed-back-by-the-endpoint";

fn token_redactor() -> Redactor {
    Redactor::default().with(TOKEN, Credential::BearerToken)
}

#[test]
fn an_echoed_credential_goes_before_the_body_is_reported() {
    let body = format!(r#"{{"error":"bad key","sent":"{TOKEN}"}}"#);
    let reported = reported_body(body.as_bytes(), &token_redactor());
    assert!(!reported.contains(TOKEN), "{reported}");
    assert!(reported.contains("[redacted bearer token]"), "{reported}");
    assert!(reported.contains("bad key"), "{reported}");
}

#[test]
fn a_credential_severed_by_the_length_limit_goes_too() {
    // The size cap cuts the body before redaction sees it, so the half that
    // survived the cut is what would otherwise be reported. Marking runs after
    // cleaning for exactly this: `[truncated]` would hide the tail from the
    // check that removes it.
    let severed = &TOKEN[..24];
    let reported = truncated_body(
        format!(r#"{{"sent":"{severed}"#).as_bytes(),
        &token_redactor(),
    );
    assert!(!reported.contains(severed), "{reported}");
    assert!(reported.contains("[redacted bearer token]"), "{reported}");
    assert!(reported.ends_with("\n[truncated]"), "{reported}");
}

#[test]
fn a_reader_tells_redaction_from_truncation() {
    // Both mark themselves, and they mean different things: one says a credential
    // was here, the other says the body ran long.
    let reported = truncated_body(b"{\"error\":\"rate_limit_error\"}", &token_redactor());
    assert_eq!(reported, "{\"error\":\"rate_limit_error\"}\n[truncated]");
    assert!(!reported.contains("[redacted"), "{reported}");
}

#[tokio::test]
async fn a_response_that_was_never_a_stream_reports_no_credential() {
    // A 200 whose body is a gateway's error page rather than SSE. A gateway that
    // echoes the request it bounced hands back the Authorization header with it,
    // and the preview of that page is reported the way a refusal body is.
    let page = format!("<html>rejected upstream: Bearer {TOKEN}</html>\n");
    let mut stream = redacting_stream(Cursor::new(page.into_bytes()), token_redactor());
    let error = stream.next().await.unwrap().unwrap_err();
    let text = error.to_string();
    assert!(text.contains("was not an SSE stream"), "{text}");
    assert!(!text.contains(TOKEN), "{text}");
    assert!(text.contains("[redacted bearer token]"), "{text}");
}

#[tokio::test]
async fn a_gateway_page_that_wrapped_the_credential_reports_neither_half() {
    // The leak an adversarial review found: `record_non_sse` joins lines with a
    // space, so a token the page wrapped was verbatim in neither half and sat at
    // the end of neither, and the whole of it reached the summary.
    let page = format!(
        "<pre>rejected: authorization: Bearer {}\n{}</pre>\n",
        &TOKEN[..24],
        &TOKEN[24..]
    );
    let mut stream = redacting_stream(Cursor::new(page.into_bytes()), token_redactor());
    let text = stream.next().await.unwrap().unwrap_err().to_string();
    assert!(text.contains("was not an SSE stream"), "{text}");
    assert!(!text.contains(&TOKEN[..24]), "{text}");
    assert!(!text.contains(&TOKEN[24..]), "{text}");
    // Removing the separator the join inserted must not put the token back.
    assert!(!text.replace(' ', "").contains(TOKEN), "{text}");
}
