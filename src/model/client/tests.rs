use super::sse::{OpenAiDecoder, decoded_stream};
use super::*;
use futures::StreamExt;
use std::io;
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
    decoded_stream(reader, Box::new(OpenAiDecoder))
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
