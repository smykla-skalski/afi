//! Tests for the `thinking` parameter's three states and for what afi will and
//! will not replay out of history.

use super::*;

fn signed(text: &str) -> Value {
    json!({"type": "thinking", "thinking": text, "signature": "sig"})
}

// --- resolving the request parameter -----------------------------------------

#[test]
fn absent_thinking_resolves_to_an_explicit_disabled() {
    // Explicit rather than omitted: thinking is on by default on Opus 5,
    // Sonnet 5, and Fable 5, and Haiku 4.5 rejects adaptive outright.
    assert_eq!(resolve(None), Some(json!({"type": "disabled"})));
    let unrelated = json!({"service_tier": "auto"});
    assert_eq!(resolve(Some(&unrelated)), Some(json!({"type": "disabled"})));
}

#[test]
fn null_omits_the_key_entirely() {
    // The only way to reach Claude Fable 5, which rejects an explicit disabled.
    let extra = json!({"thinking": null});
    assert_eq!(resolve(Some(&extra)), None);
}

#[test]
fn an_object_is_passed_through_verbatim() {
    let extra = json!({"thinking": {"type": "adaptive", "display": "summarized"}});
    assert_eq!(
        resolve(Some(&extra)),
        Some(json!({"type": "adaptive", "display": "summarized"}))
    );
}

// --- replay mode ---------------------------------------------------------------

#[test]
fn only_an_explicit_disabled_drops_stored_blocks() {
    assert_eq!(mode(Some(&json!({"type": "disabled"}))), Thinking::Drop);
    assert_eq!(mode(Some(&json!({"type": "adaptive"}))), Thinking::Replay);
    // Omitted means the model decides, and every model that accepts omission
    // thinks by default.
    assert_eq!(mode(None), Thinking::Replay);
}

#[test]
fn thinking_disabled_tracks_the_resolved_value() {
    assert!(thinking_disabled(None));
    assert!(!thinking_disabled(Some(&json!({"thinking": null}))));
    assert!(!thinking_disabled(Some(
        &json!({"thinking": {"type": "adaptive"}})
    )));
    assert!(thinking_disabled(Some(
        &json!({"thinking": {"type": "disabled"}})
    )));
}

// --- what is replayable --------------------------------------------------------

#[test]
fn stored_blocks_keep_signed_thinking_and_redacted_payloads() {
    let message = json!({
        "role": "assistant",
        THINKING_HISTORY_KEY: [
            signed("planning"),
            {"type": "redacted_thinking", "data": "encrypted"},
        ],
    });
    assert_eq!(
        stored_blocks(&message),
        vec![
            signed("planning"),
            json!({"type": "redacted_thinking", "data": "encrypted"}),
        ]
    );
}

#[test]
fn an_empty_thinking_text_is_still_replayable() {
    // `display: "omitted"` is the default, and returns blocks whose text is an
    // empty string. The signature is what the API verifies.
    let message = json!({"role": "assistant", THINKING_HISTORY_KEY: [signed("")]});
    assert_eq!(stored_blocks(&message), vec![signed("")]);
}

#[test]
fn unsignable_or_unknown_blocks_are_dropped() {
    let message = json!({
        "role": "assistant",
        THINKING_HISTORY_KEY: [
            {"type": "thinking", "thinking": "no signature"},
            {"type": "thinking", "thinking": "blank signature", "signature": ""},
            {"type": "redacted_thinking"},
            {"type": "text", "text": "not thinking"},
            "junk",
        ],
    });
    assert!(stored_blocks(&message).is_empty());
}

#[test]
fn a_turn_without_thinking_has_no_blocks() {
    let message = json!({"role": "assistant", "content": "hi"});
    assert!(stored_blocks(&message).is_empty());
}

// --- stripping for the OpenAI path ---------------------------------------------

#[test]
fn strip_history_borrows_when_nothing_carries_blocks() {
    let messages = vec![json!({"role": "user", "content": "hi"})];
    assert!(matches!(strip_history(&messages), Cow::Borrowed(_)));
}

#[test]
fn strip_history_removes_the_key_and_leaves_the_rest_intact() {
    let messages = vec![
        json!({"role": "user", "content": "hi"}),
        json!({
            "role": "assistant",
            "content": Value::Null,
            "tool_calls": [{"id": "call_1"}],
            THINKING_HISTORY_KEY: [signed("planning")],
        }),
    ];
    let stripped = strip_history(&messages);
    assert!(matches!(stripped, Cow::Owned(_)));
    assert_eq!(stripped[0], messages[0]);
    assert!(stripped[1].get(THINKING_HISTORY_KEY).is_none());
    assert_eq!(stripped[1]["tool_calls"], messages[1]["tool_calls"]);
}
