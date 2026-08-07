//! Request-body tests. These exist mainly to pin the one invariant nothing
//! downstream can catch: afi's own `afi_thinking` key must never reach an
//! endpoint on this protocol.

use super::*;
use crate::model::client::THINKING_HISTORY_KEY;
use serde_json::json;

fn source() -> Source {
    Source::new(
        "local",
        "http://127.0.0.1:8080/v1".to_string(),
        Some("key".to_string()),
        None,
        None,
        None,
    )
}

/// An assistant turn as `turn_finalize` writes it once Anthropic thinking is
/// on. A session that switched from an Anthropic source to this one carries
/// exactly this shape.
fn history() -> Vec<Value> {
    vec![
        json!({"role": "user", "content": "read it"}),
        json!({
            "role": "assistant",
            "content": Value::Null,
            "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": {"name": "read_file", "arguments": "{}"},
            }],
            THINKING_HISTORY_KEY: [
                {"type": "thinking", "thinking": "check the file", "signature": "sig"},
            ],
        }),
        json!({"role": "tool", "tool_call_id": "call_1", "content": "contents"}),
    ]
}

fn assert_thinking_stripped(body: &Value) {
    let messages = body["messages"].as_array().expect("messages is an array");
    assert_eq!(messages.len(), 3, "no message may be dropped");
    for message in messages {
        assert!(
            message.get(THINKING_HISTORY_KEY).is_none(),
            "OpenAI rejects unrecognized message fields: {message}"
        );
    }
    // Everything the protocol does understand survives.
    assert_eq!(messages[1]["tool_calls"][0]["id"], "call_1");
    assert_eq!(messages[2]["tool_call_id"], "call_1");
}

#[test]
fn the_streaming_body_strips_thinking_blocks() {
    let source = source();
    let body = stream_body(&StreamRequest {
        source: &source,
        model: "qwen3",
        messages: &history(),
        tools: None,
        tool_choice: None,
        max_tokens: None,
        extra_body: None,
        recovery_sampling: false,
    });
    assert_thinking_stripped(&body);
}

#[test]
fn the_completion_body_strips_thinking_blocks() {
    assert_thinking_stripped(&completion_body("qwen3", &history(), None));
}

#[test]
fn a_history_without_thinking_is_passed_through_unchanged() {
    let messages = vec![json!({"role": "user", "content": "hi"})];
    let body = completion_body("qwen3", &messages, None);
    assert_eq!(body["messages"], json!(messages));
}

#[test]
fn optional_request_fields_are_omitted_rather_than_sent_empty() {
    // A zero max_tokens means "unset" upstream, and not every backend accepts
    // a null tools array.
    let source = source();
    let body = stream_body(&StreamRequest {
        source: &source,
        model: "qwen3",
        messages: &[json!({"role": "user", "content": "hi"})],
        tools: None,
        tool_choice: None,
        max_tokens: Some(0),
        extra_body: Some(&json!({"provider": {"order": ["deepinfra"]}})),
        recovery_sampling: false,
    });
    assert!(body.get("tools").is_none());
    assert!(body.get("tool_choice").is_none());
    assert!(body.get("max_tokens").is_none());
    // extra_body merges at the top level, unwrapped.
    assert_eq!(body["provider"], json!({"order": ["deepinfra"]}));
    assert_eq!(body["stream"], true);
}

#[test]
fn openais_own_host_gets_the_output_limit_it_accepts() {
    // Reasoning models - the only ones `reasoning_effort` applies to - reject
    // `max_tokens` outright and take `max_completion_tokens`. Sending the older
    // key there 400s the first turn of every run.
    let openai = Source::new(
        "oa",
        "https://api.openai.com/v1".to_string(),
        Some("key".to_string()),
        None,
        None,
        None,
    );
    let request = |source: &Source| {
        stream_body(&StreamRequest {
            source,
            model: "gpt-5",
            messages: &[json!({"role": "user", "content": "hi"})],
            tools: None,
            tool_choice: None,
            max_tokens: Some(16_000),
            extra_body: None,
            recovery_sampling: false,
        })
    };
    let body = request(&openai);
    assert_eq!(body["max_completion_tokens"], 16_000);
    assert!(body.get("max_tokens").is_none());

    // Every other endpoint keeps the OpenAI-compatible spelling, which is the
    // only one a self-hosted server implements.
    let body = request(&source());
    assert_eq!(body["max_tokens"], 16_000);
    assert!(body.get("max_completion_tokens").is_none());
}
