use super::auto::{Decision, decide};
use super::*;
use serde_json::{Value, json};

fn msg(role: &str, content: &str) -> Value {
    json!({"role": role, "content": content})
}

fn tc_msg(id: &str, name: &str, args: &str) -> Value {
    json!({"role": "assistant", "tool_calls": [{"id": id, "function": {"name": name, "arguments": args}}]})
}

fn tool_result(id: &str, content: &str) -> Value {
    json!({"role": "tool", "tool_call_id": id, "content": content})
}

/// A system message plus `turns` user/assistant pairs.
fn conversation(turns: usize) -> Vec<Value> {
    let mut messages = vec![json!({"role": "system", "content": "sys"})];
    for i in 0..turns {
        messages.push(msg("user", &format!("turn {i}")));
        messages.push(msg("assistant", &format!("reply {i}")));
    }
    messages
}

/// Plan and apply in one step, which is what the two halves do either side of a
/// live request. `None` when there was nothing to fold.
fn fold(
    messages: &mut Vec<Value>,
    keep: usize,
    auto: bool,
    summary: &str,
) -> Option<CompressResult> {
    let plan = plan_compression(messages, keep, auto)?;
    plan.apply(messages, summary)
}

#[test]
fn a_conversation_no_longer_than_the_kept_turns_does_not_fold() {
    let mut messages = conversation(1);
    assert!(fold(&mut messages, COMPRESS_KEEP, false, "summary").is_none());
}

#[test]
fn the_head_is_summarized_and_the_tail_kept() {
    let mut messages = conversation(3);
    let plan = plan_compression(&messages, COMPRESS_KEEP, false).expect("7 messages fold");
    assert!(plan.prompt().contains("turn 0"));
    let result = plan
        .apply(&mut messages, "summary of earlier")
        .expect("applied");

    assert_eq!(result.summarized_n, 4); // 4 head messages summarized
    // system + summary + the last 2 turns
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
fn an_automatic_fold_keeps_roughly_the_last_third() {
    let mut messages = conversation(20);
    // 1 system + 40 body = 41 total. body_len=40, keep = max(2, 40/3) = 13
    assert!(fold(&mut messages, COMPRESS_KEEP, true, "summary").is_some());
    // ~13 of 40 body messages + system + summary = ~15
    assert!(messages.len() <= 16);
    assert!(messages.len() >= 14);
}

#[test]
fn a_tail_starting_on_an_orphan_tool_result_drops_it_into_the_summary() {
    // Five messages with `keep = 2` puts the split at index 3, so the tail really
    // does start on the `tool` turn whose assistant(tool_calls) parent is cut into
    // the head - the shape no chat template can render. The earlier version of
    // this test split at index 4 and left the tool result in the head, so it
    // passed with the trim stubbed out to a no-op.
    let mut messages = vec![
        json!({"role": "system", "content": "sys"}),
        msg("user", "do task"),
        tc_msg("call_1", "read_file", r#"{"path":"x"}"#),
        tool_result("call_1", "the file content"),
        msg("assistant", "done"),
    ];
    let plan = plan_compression(&messages, COMPRESS_KEEP, false).expect("5 messages fold");
    // Dropped from the tail means folded into the summary, not deleted: a turn
    // that leaves the conversation without reaching the prompt is gone for good,
    // and the model is then summarizing a history it was never shown.
    assert!(
        plan.prompt().contains("the file content"),
        "the trimmed tool result must reach the summary prompt: {}",
        plan.prompt()
    );
    let result = plan.apply(&mut messages, "summary").expect("applied");

    assert_eq!(result.summarized_n, 3, "user + tool_calls + tool result");
    assert_eq!(
        result.kept_n, 1,
        "only the assistant turn survives verbatim"
    );
    for m in &messages {
        assert!(
            m.get("role").and_then(|r| r.as_str()) != Some("tool"),
            "an orphan tool result must not survive into the tail"
        );
    }
}

#[test]
fn a_tail_trimmed_away_entirely_still_summarizes_what_it_held() {
    // A turn that ends on a batch of parallel tool results: `keep` lands inside
    // the batch, every kept turn is an orphan, and the whole tail is trimmed. All
    // of it has to end up in the summary.
    let mut messages = vec![
        json!({"role": "system", "content": "sys"}),
        msg("user", "do task"),
        tc_msg("call_1", "read_file", r#"{"path":"x"}"#),
        tool_result("call_1", "first result"),
        tool_result("call_2", "second result"),
    ];
    let plan = plan_compression(&messages, COMPRESS_KEEP, false).expect("5 messages fold");
    for held in ["first result", "second result"] {
        assert!(
            plan.prompt().contains(held),
            "{held} must reach the summary prompt: {}",
            plan.prompt()
        );
    }
    let result = plan.apply(&mut messages, "summary").expect("applied");
    assert_eq!(result.kept_n, 0, "the whole tail was unrenderable");
    assert_eq!(result.summarized_n, 4, "every body turn is in the summary");
    // system + summary, and nothing else.
    assert_eq!(messages.len(), 2);
}

#[test]
fn an_empty_summary_leaves_the_conversation_alone() {
    let mut messages = conversation(3);
    let before = messages.clone();
    assert!(fold(&mut messages, COMPRESS_KEEP, false, "   ").is_none());
    assert_eq!(messages, before);
}

#[test]
fn a_plan_that_is_never_applied_changes_nothing() {
    // The wait between planning and the summary coming back is where an Esc
    // lands, and a conversation folded halfway is not one the run can carry on
    // from.
    let messages = conversation(20);
    let before = messages.clone();
    let plan = plan_compression(&messages, COMPRESS_KEEP, true).expect("a long chat folds");
    assert!(plan.prompt().contains("turn 0"));
    drop(plan);
    assert_eq!(messages, before);
}

#[test]
fn folding_is_off_at_zero_percent() {
    assert_eq!(decide(100_000, 0, Some(200_000)), Decision::Keep);
}

#[test]
fn a_turn_well_under_the_threshold_is_kept() {
    // 1000/200_000 = 0.5%, nowhere near 85%
    assert_eq!(decide(1000, 85, Some(200_000)), Decision::Keep);
}

#[test]
fn a_turn_over_the_threshold_folds_against_its_window() {
    // 180_000/200_000 = 90% > 85%
    assert_eq!(
        decide(180_000, 85, Some(200_000)),
        Decision::Fold { window: 200_000 }
    );
}

#[test]
fn an_unknown_window_is_not_the_same_as_a_turn_under_the_threshold() {
    // The difference is what decides whether the run says anything: nobody knows
    // the window, rather than the window having room left.
    assert_eq!(decide(180_000, 85, None), Decision::WindowUnknown);
}

#[test]
fn a_turn_the_provider_counted_nothing_for_is_kept() {
    assert_eq!(decide(0, 85, Some(200_000)), Decision::Keep);
    // Even with no window, because there is nothing to measure either way.
    assert_eq!(decide(0, 85, None), Decision::Keep);
}

#[test]
fn a_window_declared_as_zero_never_folds_and_says_nothing() {
    // `AFI_SOURCE_X_CONTEXT_WINDOW=0` turns folding off for one source without
    // touching the percentage, so it is an answer rather than a gap.
    assert_eq!(decide(u64::MAX, 85, Some(0)), Decision::Keep);
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
    fold(&mut messages, COMPRESS_KEEP, false, "summary")
        .expect("this history is long enough to compress");

    assert_eq!(messages[0], system, "sent unchanged after a fold");
    assert!(
        messages[1..].iter().all(|m| m["role"] != "system"),
        "and only once: {messages:?}"
    );
}
