use super::*;
use serde_json::json;

fn msg(role: &str, content: &str) -> Value {
    json!({"role": role, "content": content})
}

fn tc_msg(id: &str, name: &str, args: &str) -> Value {
    json!({"role": "assistant", "tool_calls": [{"id": id, "function": {"name": name, "arguments": args}}]})
}

fn tool_result(id: &str, content: &str) -> Value {
    json!({"role": "tool", "tool_call_id": id, "content": content})
}

fn conversation(turns: usize) -> Vec<Value> {
    let mut messages = vec![json!({"role": "system", "content": "sys"})];
    for i in 0..turns {
        messages.push(msg("user", &format!("turn {i}")));
        messages.push(msg("assistant", &format!("reply {i}")));
    }
    messages
}

#[test]
fn compress_too_short_returns_none() {
    let mut messages = vec![
        json!({"role": "system", "content": "sys"}),
        msg("user", "hi"),
        msg("assistant", "hello"),
    ];
    let result = compress(&mut messages, COMPRESS_KEEP, false, |_| {
        Some("summary".to_string())
    });
    assert!(result.is_none());
}

#[test]
fn compress_summarizes_head_and_keeps_tail() {
    let mut messages = vec![
        json!({"role": "system", "content": "sys"}),
        msg("user", "do task"),
        msg("assistant", "ok"),
        msg("user", "step 2"),
        msg("assistant", "done"),
        msg("user", "step 3"),
        msg("assistant", "done"),
    ];
    let result = compress(&mut messages, COMPRESS_KEEP, false, |prompt| {
        assert!(prompt.contains("do task"));
        Some("summary of earlier".to_string())
    });
    let result = result.unwrap();
    assert_eq!(result.summarized_n, 4); // 4 head messages summarized
    // Should have system + summary + last 2 turns
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[1]["role"], "user");
    assert!(
        messages[1]["content"]
            .as_str()
            .unwrap()
            .contains("summary of earlier")
    );
}

#[test]
fn compress_auto_keeps_third() {
    let mut messages = conversation(20);
    // 1 system + 40 body = 41 total. body_len=40, keep = max(2, 40/3) = 13
    let result = compress(&mut messages, COMPRESS_KEEP, true, |_| {
        Some("summary".to_string())
    });
    assert!(result.is_some());
    // Should keep ~13 of 40 body messages + system + summary = ~15
    assert!(messages.len() <= 16);
    assert!(messages.len() >= 14);
}

#[test]
fn compress_drops_leading_tool_from_tail() {
    let mut messages = vec![
        json!({"role": "system", "content": "sys"}),
        msg("user", "do task"),
        tc_msg("call_1", "read_file", r#"{"path":"x"}"#),
        tool_result("call_1", "file content"),
        // tail starts here (keep=2) but the first is a tool result with
        // no preceding assistant(tool_calls) in the tail
        msg("user", "next"),
        msg("assistant", "done"),
    ];
    let result = compress(&mut messages, COMPRESS_KEEP, false, |_| {
        Some("summary".to_string())
    });
    // compress returns Some but the tool result must be dropped from the tail
    assert!(result.is_some());
    // Check no tool message is in the result
    for m in &messages {
        assert!(
            m.get("role").and_then(|r| r.as_str()) != Some("tool"),
            "tool message should have been dropped from tail"
        );
    }
}

#[test]
fn compress_returns_none_on_empty_summary() {
    let mut messages = vec![
        json!({"role": "system", "content": "sys"}),
        msg("user", "do task"),
        msg("assistant", "ok"),
        msg("user", "step 2"),
        msg("assistant", "done"),
        msg("user", "step 3"),
        msg("assistant", "done"),
    ];
    let result = compress(&mut messages, COMPRESS_KEEP, false, |_| Some(String::new()));
    assert!(result.is_none());
}

#[test]
fn a_plan_that_is_never_applied_changes_nothing() {
    // The half-second between planning and the summary coming back is where an
    // Esc lands, and a conversation that was folded halfway is not one the run
    // can carry on from.
    let mut messages = conversation(20);
    let before = messages.clone();
    let plan = plan_compression(&messages, COMPRESS_KEEP, true).expect("a long chat folds");
    assert!(plan.prompt().contains("turn 0"));
    drop(plan);
    assert_eq!(messages, before);
    // And the same plan, applied, leaves the system message in place.
    let plan = plan_compression(&messages, COMPRESS_KEEP, true).expect("a long chat folds");
    let result = plan.apply(&mut messages, "the summary").expect("applied");
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(result.summary_chars, "the summary".len());
}

#[test]
fn maybe_autocompress_disabled() {
    let result = maybe_autocompress(100_000, 0, Some(200_000));
    assert!(!result);
}

#[test]
fn maybe_autocompress_below_threshold() {
    // 1000/200_000 = 0.5% << 85%
    assert!(!maybe_autocompress(1000, 85, Some(200_000)));
}

#[test]
fn maybe_autocompress_above_threshold() {
    // 180_000/200_000 = 90% > 85%
    assert!(maybe_autocompress(180_000, 85, Some(200_000)));
}

#[test]
fn maybe_autocompress_no_context_window() {
    assert!(!maybe_autocompress(180_000, 85, None));
}

#[test]
fn maybe_autocompress_zero_prompt_tokens() {
    assert!(!maybe_autocompress(0, 85, Some(200_000)));
}

#[test]
fn a_window_declared_as_zero_never_folds() {
    // `AFI_SOURCE_X_CONTEXT_WINDOW=0` is how an operator turns folding off for
    // one source without touching the percentage.
    assert!(!maybe_autocompress(u64::MAX, 85, Some(0)));
}

#[test]
fn compression_keeps_the_system_message_whole() {
    // The project instructions a run loads ride inside the system message, so
    // this is what keeps them from being folded into a summary of themselves.
    // Harnesses that put them in a user message have to re-read the file and
    // re-inject after every compaction; afi has nothing to re-inject because
    // nothing was dropped. Asserted on the whole string rather than on the role,
    // since a summarized copy would still be role `system`.
    //
    // This covers the fold auto-compress performs. `/compress` folds through
    // `repl::commands::apply_compression`, which holds the same property by
    // construction - it pushes `messages[0]` unchanged - and is private to that
    // module, so it cannot be asserted on from here.
    let system =
        json!({"role": "system", "content": "afi rules\n\nContents of /r/AGENTS.md:\n\nuse mise"});
    let mut messages = vec![system.clone()];
    for turn in ["one", "two", "three", "four", "five", "six"] {
        messages.push(msg("user", turn));
    }
    compress(&mut messages, COMPRESS_KEEP, false, |_| {
        Some("summary".to_string())
    })
    .expect("this history is long enough to compress");

    assert_eq!(messages[0], system, "sent unchanged after a fold");
    assert!(
        messages[1..].iter().all(|m| m["role"] != "system"),
        "and only once: {messages:?}"
    );
}
