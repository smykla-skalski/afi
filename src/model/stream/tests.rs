use super::*;

#[test]
fn normalize_usage_openai() {
    let usage = Usage {
        prompt_tokens: 1000,
        completion_tokens: 500,
        prompt_tokens_details: Some(PromptTokensDetails {
            cached_tokens: 200,
            ..PromptTokensDetails::default()
        }),
        output_tokens_details: Some(OutputTokensDetails {
            reasoning_tokens: 50,
        }),
    };
    let n = normalize_usage(Some(&usage), None, 0).unwrap();
    assert_eq!(n.input_tokens, 800); // 1000 - 200 cache
    assert_eq!(n.output_tokens, 450); // 500 - 50 reasoning
    assert_eq!(n.cache_read_tokens, 200);
    assert_eq!(n.reasoning_tokens, 50);
    // No OpenAI-compatible provider reports a cache write, so this is 0 rather
    // than a guess derived from the other counts.
    assert_eq!(n.cache_write_tokens, 0);
}

#[test]
fn normalize_usage_splits_cache_writes_out_of_input() {
    // The Anthropic shape: prompt_tokens is the whole context, and both cache
    // subsets sit inside it. A write must not land in input_tokens, which is
    // priced below it.
    let usage = Usage {
        prompt_tokens: 1050,
        completion_tokens: 42,
        prompt_tokens_details: Some(PromptTokensDetails {
            cached_tokens: 900,
            cache_write_tokens: 50,
        }),
        output_tokens_details: None,
    };
    let n = normalize_usage(Some(&usage), None, 0).unwrap();
    assert_eq!(n.input_tokens, 100); // 1050 - 900 read - 50 write
    assert_eq!(n.cache_read_tokens, 900);
    assert_eq!(n.cache_write_tokens, 50);
    assert_eq!(n.output_tokens, 42);
    // Disjoint, so the five counts still add back up to the whole request.
    assert_eq!(
        n.input_tokens + n.cache_read_tokens + n.cache_write_tokens + n.output_tokens,
        1092
    );
}

#[test]
fn normalize_usage_saturates_when_cache_subsets_exceed_the_prompt() {
    // A provider reporting subsets larger than the total it claims must floor
    // input at zero rather than wrapping to something enormous.
    let usage = Usage {
        prompt_tokens: 10,
        completion_tokens: 1,
        prompt_tokens_details: Some(PromptTokensDetails {
            cached_tokens: 900,
            cache_write_tokens: 50,
        }),
        output_tokens_details: None,
    };
    let n = normalize_usage(Some(&usage), None, 0).unwrap();
    assert_eq!(n.input_tokens, 0);
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
    // llama.cpp's `cache_n` is a prefix-reuse hit, not a write, so there is
    // nothing to report here.
    assert_eq!(n.cache_write_tokens, 0);
}

#[test]
fn normalize_usage_fallback_chars() {
    let n = normalize_usage(None, None, 8000).unwrap();
    assert_eq!(n.output_tokens, 2000); // 8000 / 4
    assert_eq!(n.input_tokens, 0);
    assert_eq!(n.cache_write_tokens, 0);
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
fn parse_sse_line_accepts_data_without_space() {
    let line = r#"data:{"choices":[{"delta":{"content":"hello"}}]}"#;
    let chunk = parse_sse_line(line).unwrap();
    assert_eq!(chunk.content.as_deref(), Some("hello"));
}

#[test]
fn live_decoder_reports_malformed_json() {
    assert!(decode_sse_line("data: {broken").is_err());
}

#[test]
fn live_decoder_reports_provider_error_payload() {
    let error = decode_sse_line(r#"data: {"error":{"message":"overloaded"}}"#)
        .expect_err("provider error must fail the stream");
    assert!(error.to_string().contains("overloaded"));
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

#[test]
fn parse_sse_body_joins_data_fields() {
    let body = "data: {\"choices\":[\ndata: {\"delta\":{\"content\":\"a\"}}\ndata: ]}\n\n";
    let chunks = parse_sse_body(body);
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].content.as_deref(), Some("a"));
}
