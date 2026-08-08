use super::*;
use crate::model::stream::{Usage, normalize_usage};
use crate::model::usage_totals::{RefusedToolCalls, UsageTotals};
use crate::summary::{RunAuth, RunSummary};
use crate::tools::known_tool_names;

/// Decode one event payload, expecting a chunk.
fn chunk(decoder: &mut AnthropicDecoder, data: &str) -> StreamChunk {
    match decoder.decode(data) {
        Ok(SseLine::Chunk(c)) => *c,
        other => panic!("expected a chunk, got {other:?}"),
    }
}

fn is_ignored(decoder: &mut AnthropicDecoder, data: &str) -> bool {
    matches!(decoder.decode(data), Ok(SseLine::Ignore))
}

#[test]
fn text_delta_becomes_content() {
    let mut d = AnthropicDecoder::new();
    let c = chunk(
        &mut d,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello"}}"#,
    );
    assert_eq!(c.content.as_deref(), Some("hello"));
    assert!(c.reasoning_content.is_none());
    assert!(c.tool_calls.is_empty());
}

#[test]
fn thinking_delta_feeds_both_the_display_and_replay_copies() {
    let mut d = AnthropicDecoder::new();
    let c = chunk(
        &mut d,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"pondering"}}"#,
    );
    assert_eq!(c.reasoning_content.as_deref(), Some("pondering"));
    assert!(c.content.is_none());
    assert_eq!(c.thinking.len(), 1);
    assert_eq!(c.thinking[0].index, 0);
    assert_eq!(c.thinking[0].thinking.as_deref(), Some("pondering"));
    assert!(c.thinking[0].signature.is_none());
}

#[test]
fn signature_delta_is_replay_only() {
    // Not displayable, but the API verifies it when the block is echoed back.
    let mut d = AnthropicDecoder::new();
    let c = chunk(
        &mut d,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"abc"}}"#,
    );
    assert!(c.reasoning_content.is_none());
    assert!(c.content.is_none());
    assert_eq!(c.thinking.len(), 1);
    assert_eq!(c.thinking[0].index, 0);
    assert_eq!(c.thinking[0].signature.as_deref(), Some("abc"));
}

#[test]
fn redacted_thinking_block_start_carries_its_payload() {
    let mut d = AnthropicDecoder::new();
    let c = chunk(
        &mut d,
        r#"{"type":"content_block_start","index":1,"content_block":{"type":"redacted_thinking","data":"encrypted"}}"#,
    );
    assert!(c.reasoning_content.is_none());
    assert_eq!(c.thinking.len(), 1);
    assert_eq!(c.thinking[0].index, 1);
    assert_eq!(c.thinking[0].redacted.as_deref(), Some("encrypted"));
}

#[test]
fn tool_use_block_start_opens_a_tool_call() {
    let mut d = AnthropicDecoder::new();
    let c = chunk(
        &mut d,
        r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"read_file","input":{}}}"#,
    );
    assert_eq!(c.tool_calls.len(), 1);
    let tc = &c.tool_calls[0];
    // Anthropic's index counts all blocks, so a tool after a text block is 1.
    assert_eq!(tc.index, 1);
    assert_eq!(tc.id.as_deref(), Some("toolu_1"));
    assert_eq!(tc.name.as_deref(), Some("read_file"));
    assert!(tc.arguments.is_none());
}

#[test]
fn text_and_thinking_block_starts_are_ignored() {
    let mut d = AnthropicDecoder::new();
    assert!(is_ignored(
        &mut d,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#
    ));
    assert!(is_ignored(
        &mut d,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#
    ));
}

#[test]
fn input_json_delta_accumulates_arguments_under_its_index() {
    let mut d = AnthropicDecoder::new();
    let first = chunk(
        &mut d,
        r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}"#,
    );
    let second = chunk(
        &mut d,
        r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"\"a.rs\"}"}}"#,
    );
    assert_eq!(first.tool_calls[0].index, 2);
    assert_eq!(first.tool_calls[0].arguments.as_deref(), Some("{\"path\":"));
    assert_eq!(second.tool_calls[0].index, 2);
    assert_eq!(second.tool_calls[0].arguments.as_deref(), Some("\"a.rs\"}"));
    // Fragments must not carry id/name, or they would clobber the open call.
    assert!(second.tool_calls[0].id.is_none());
    assert!(second.tool_calls[0].name.is_none());
}

const SPLIT_USAGE_START: &str = r#"{"type":"message_start","message":{"usage":{"input_tokens":100,"cache_read_input_tokens":900,"cache_creation_input_tokens":50,"output_tokens":1}}}"#;
const SPLIT_USAGE_DELTA: &str =
    r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":42}}"#;

/// Drive the two-event usage sequence and return the merged result.
fn merged_usage() -> Usage {
    let mut d = AnthropicDecoder::new();
    assert!(is_ignored(&mut d, SPLIT_USAGE_START));
    chunk(&mut d, SPLIT_USAGE_DELTA)
        .usage
        .expect("message_delta must carry merged usage")
}

#[test]
fn usage_is_merged_across_message_start_and_message_delta() {
    let usage = merged_usage();
    // Anthropic's input_tokens excludes both cache counts, so prompt_tokens
    // re-inflates it to the whole context - what the stats footer renders as
    // `ctx`, and what a cold first turn would otherwise under-report.
    let details = usage.prompt_tokens_details.as_ref().expect("details");
    assert_eq!(
        (
            usage.prompt_tokens,
            usage.completion_tokens,
            details.cached_tokens,
            details.cache_write_tokens
        ),
        (1050, 42, 900, 50)
    );
}

#[test]
fn merged_usage_normalizes_the_way_the_footer_expects() {
    // A cache write is priced above plain input, so it comes out of
    // input_tokens and is reported on its own rather than folded in.
    let norm = normalize_usage(Some(&merged_usage()), None, 0).expect("normalizes");
    assert_eq!(
        (
            norm.input_tokens,
            norm.cache_read_tokens,
            norm.cache_write_tokens,
            norm.output_tokens,
            norm.reasoning_tokens
        ),
        (100, 900, 50, 42, 0)
    );
}

#[test]
fn message_start_without_usage_leaves_counters_at_zero() {
    let mut d = AnthropicDecoder::new();
    assert!(is_ignored(&mut d, r#"{"type":"message_start"}"#));
    let c = chunk(
        &mut d,
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#,
    );
    let usage = c.usage.expect("usage present");
    assert_eq!(usage.prompt_tokens, 0);
    assert_eq!(usage.completion_tokens, 0);
}

#[test]
fn stop_reasons_normalize_to_openai_vocabulary() {
    for (anthropic, expected) in [
        ("end_turn", "stop"),
        ("stop_sequence", "stop"),
        ("pause_turn", "stop"),
        ("tool_use", "tool_calls"),
        ("max_tokens", "length"),
        ("model_context_window_exceeded", "length"),
    ] {
        let mut d = AnthropicDecoder::new();
        let data = format!(r#"{{"type":"message_delta","delta":{{"stop_reason":"{anthropic}"}}}}"#);
        let c = chunk(&mut d, &data);
        assert_eq!(
            c.finish_reason.as_deref(),
            Some(expected),
            "{anthropic} should map to {expected}"
        );
    }
}

#[test]
fn truncation_stop_reasons_satisfy_the_forced_final_check() {
    // turn_finalize gates the forced-final path on `.contains("length")`.
    let mut d = AnthropicDecoder::new();
    let c = chunk(
        &mut d,
        r#"{"type":"message_delta","delta":{"stop_reason":"max_tokens"}}"#,
    );
    assert!(c.finish_reason.as_deref().unwrap().contains("length"));
}

#[test]
fn unknown_stop_reason_passes_through_verbatim() {
    let mut d = AnthropicDecoder::new();
    let c = chunk(
        &mut d,
        r#"{"type":"message_delta","delta":{"stop_reason":"some_future_reason"}}"#,
    );
    assert_eq!(c.finish_reason.as_deref(), Some("some_future_reason"));
}

#[test]
fn message_stop_ends_the_stream() {
    let mut d = AnthropicDecoder::new();
    assert!(matches!(
        d.decode(r#"{"type":"message_stop"}"#),
        Ok(SseLine::Done)
    ));
}

#[test]
fn ping_and_block_stop_and_unknown_types_are_ignored() {
    let mut d = AnthropicDecoder::new();
    assert!(is_ignored(&mut d, r#"{"type":"ping"}"#));
    assert!(is_ignored(
        &mut d,
        r#"{"type":"content_block_stop","index":0}"#
    ));
    assert!(is_ignored(&mut d, r#"{"type":"future_event_type"}"#));
    assert!(is_ignored(&mut d, "{}"));
}

#[test]
fn top_level_error_becomes_a_provider_error() {
    let mut d = AnthropicDecoder::new();
    let err = d
        .decode(r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#)
        .expect_err("error event must fail the stream");
    let text = err.to_string();
    assert!(text.contains("overloaded_error"), "got {text}");
    assert!(text.contains("Overloaded"), "got {text}");
}

#[test]
fn refusal_becomes_a_provider_error_with_its_category() {
    let mut d = AnthropicDecoder::new();
    let err = d
        .decode(
            r#"{"type":"message_delta","delta":{"stop_reason":"refusal","stop_details":{"type":"refusal","category":"cyber","explanation":"declined"}}}"#,
        )
        .expect_err("refusal must fail the stream, not read as a clean stop");
    let text = err.to_string();
    assert!(text.contains("refusal"), "got {text}");
    assert!(text.contains("cyber"), "got {text}");
    assert!(text.contains("declined"), "got {text}");
}

#[test]
fn refusal_without_stop_details_still_errors() {
    let mut d = AnthropicDecoder::new();
    let err = d
        .decode(r#"{"type":"message_delta","delta":{"stop_reason":"refusal"}}"#)
        .expect_err("refusal must fail the stream");
    assert!(err.to_string().contains("unspecified"));
}

/// The framing layer hands each `data:` field to the decoder speculatively
/// before buffering it, so a fragment of a `message_start` arrives here and must
/// fail to parse *without* having mutated the usage counters. If it mutated
/// eagerly, the re-decoded whole event would double-count.
#[test]
fn a_fragmented_event_does_not_corrupt_usage_state() {
    let mut d = AnthropicDecoder::new();
    let fragment = r#"{"type":"message_start","message":{"usage":{"input_tokens":100,"cache_re"#;
    assert!(
        matches!(d.decode(fragment), Err(SseDecodeError::Json(_))),
        "a fragment must be reported as a JSON error so the framing layer buffers it"
    );
    assert_eq!(
        counters(&d),
        (0, 0, 0),
        "state must be untouched by a failed parse"
    );

    // Now the reassembled event: applied exactly once, not double-counted.
    let whole = r#"{"type":"message_start","message":{"usage":{"input_tokens":100,"cache_read_input_tokens":5}}}"#;
    assert!(is_ignored(&mut d, whole));
    assert_eq!(counters(&d), (100, 5, 0));
}

fn counters(decoder: &AnthropicDecoder) -> (u64, u64, u64) {
    (decoder.input, decoder.cache_read, decoder.cache_creation)
}

/// One request's `message_start` / `message_delta` pair, with the usage numbers
/// Anthropic reports on each.
fn request_usage(input: u64, cache_read: u64, cache_creation: u64, output: u64) -> Usage {
    let mut d = AnthropicDecoder::new();
    let start = format!(
        r#"{{"type":"message_start","message":{{"usage":{{"input_tokens":{input},"cache_read_input_tokens":{cache_read},"cache_creation_input_tokens":{cache_creation},"output_tokens":1}}}}}}"#
    );
    assert!(is_ignored(&mut d, &start));
    let delta = format!(
        r#"{{"type":"message_delta","delta":{{"stop_reason":"end_turn"}},"usage":{{"output_tokens":{output}}}}}"#
    );
    chunk(&mut d, &delta).usage.expect("merged usage")
}

#[test]
fn a_multi_turn_run_sums_cache_writes_the_way_anthropic_reports_them() {
    // The check a pass-through proxy performs against a live run, made
    // deterministic: three requests whose cached prefix lapses and is rebuilt,
    // driven through the real decoder and folded exactly as a run is. A
    // warm-cache run reports zero creation on every request and proves nothing,
    // so the writes here are non-zero on purpose.
    let requests = [
        // cold: the whole prefix is written, nothing is read back
        request_usage(123, 0, 2279, 120),
        // warm: the prefix is read, a little more is appended and written
        request_usage(924, 2279, 310, 216),
        // lapsed: the 5-minute TTL expired, so the prefix is rebuilt in full
        request_usage(3038, 0, 2589, 173),
    ];

    let mut totals = UsageTotals::default();
    for usage in &requests {
        totals.add(&normalize_usage(Some(usage), None, 0).expect("normalizes"));
    }

    // Summed by hand from the per-request cache_creation_input_tokens above.
    assert_eq!(
        (
            totals.cache_write_tokens,
            totals.input_tokens,
            totals.cache_read_tokens,
            totals.output_tokens,
            totals.requests
        ),
        (
            2279 + 310 + 2589,
            // Anthropic's own input_tokens, unchanged - no write leaked in.
            123 + 924 + 3038,
            2279,
            120 + 216 + 173,
            3
        )
    );

    // Every token of every request is accounted for exactly once.
    let reported: u64 = requests
        .iter()
        .map(|u| u.prompt_tokens + u.completion_tokens)
        .sum();
    assert_eq!(totals.total_tokens(), reported);
}

#[test]
fn the_run_summary_reports_those_writes_separately() {
    let mut totals = UsageTotals::default();
    totals.add(&normalize_usage(Some(&request_usage(123, 0, 2279, 120)), None, 0).unwrap());
    let json = RunSummary {
        ok: true,
        error: None,
        error_kind: None,
        source: Some("anthropic"),
        model: Some("claude-sonnet-5"),
        answer: "done",
        usage: totals,
        cost_usd: None,
        elapsed_secs: 1.0,
        tools: known_tool_names().to_vec(),
        effort: None,
        refused_tool_calls: RefusedToolCalls::default(),
        auth: Some(RunAuth::ApiKey),
        // The flat counts are what this asserts on; the per-source breakdown
        // reports the same tokens and is proved where it is built.
        sources: Vec::new(),
        system_prompt_mode: Some("builtin"),
        system_prompt_file: None,
    }
    .to_json();
    assert_eq!(json["usage"]["cache_write_tokens"], 2279);
    assert_eq!(json["usage"]["input_tokens"], 123);
    assert_eq!(json["usage"]["total_tokens"], 123 + 2279 + 120);
}
