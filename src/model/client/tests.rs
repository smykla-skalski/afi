use super::*;
use futures::StreamExt;
use tokio::io::{AsyncRead, AsyncWriteExt, duplex};

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
