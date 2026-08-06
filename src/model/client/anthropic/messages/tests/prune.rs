//! Orphan pruning and the structural invariants Anthropic enforces on
//! `messages`. Split from the sibling translation tests to stay under the
//! 420-line source cap.

use super::*;

// --- orphan pruning -----------------------------------------------------------

#[test]
fn orphan_tool_result_is_pruned() {
    // apply_compression slices the last N turns and can start mid tool-cycle,
    // leaving a tool_result with no preceding tool_use. Anthropic 400s on that.
    let history = vec![
        json!({"role": "user", "content": "summary of earlier work"}),
        tool_result("gone", "orphaned output"),
        json!({"role": "assistant", "content": "carrying on"}),
    ];
    let out = translate(&history);
    assert_eq!(roles(&out.messages), vec!["user", "assistant"]);
}

#[test]
fn orphan_tool_use_is_pruned() {
    // The mirror case: history ends on an assistant tool_calls turn whose
    // results were cut away.
    let history = vec![
        json!({"role": "user", "content": "go"}),
        json!({
            "role": "assistant",
            "content": "I'll check that.",
            "tool_calls": [{
                "id": "cut", "type": "function",
                "function": {"name": "read_file", "arguments": "{}"},
            }],
        }),
    ];
    let out = translate(&history);
    let assistant = blocks(&out.messages[1]);
    assert_eq!(assistant.len(), 1);
    assert_eq!(assistant[0]["type"], "text", "only the text survives");
}

#[test]
fn an_assistant_turn_of_only_orphan_tool_uses_is_removed() {
    let history = vec![
        json!({"role": "user", "content": "go"}),
        assistant_tool_call("cut", "read_file", "{}"),
    ];
    assert_eq!(roles(&translate(&history).messages), vec!["user"]);
}

#[test]
fn partially_answered_tool_calls_keep_only_the_answered_one() {
    let history = vec![
        json!({"role": "user", "content": "go"}),
        json!({
            "role": "assistant",
            "content": Value::Null,
            "tool_calls": [
                {"id": "kept", "type": "function", "function": {"name": "read_file", "arguments": "{}"}},
                {"id": "cut", "type": "function", "function": {"name": "list_dir", "arguments": "{}"}},
            ],
        }),
        tool_result("kept", "output"),
    ];
    let out = translate(&history);
    let assistant = blocks(&out.messages[1]);
    assert_eq!(assistant.len(), 1);
    assert_eq!(assistant[0]["id"], "kept");
}

// --- synthetic id alignment ---------------------------------------------------

#[test]
fn a_dropped_nameless_call_does_not_shift_its_sibling_s_result() {
    // turn_finalize writes `name` with unwrap_or_default(), so an accumulator
    // that never got a name yields `"name": ""` - and turn_dispatch still
    // dispatches it and pushes a result. The nameless call is dropped here, but
    // it must still consume a synthetic id, or its result binds to the *next*
    // call and the real result is pruned. That is a silently wrong tool result.
    let history = vec![
        json!({"role": "user", "content": "go"}),
        json!({
            "role": "assistant",
            "content": Value::Null,
            "tool_calls": [
                {"id": "", "type": "function", "function": {"name": "", "arguments": "{}"}},
                {"id": "", "type": "function",
                 "function": {"name": "read_file", "arguments": "{\"path\":\"secret.txt\"}"}},
            ],
        }),
        tool_result("", "ERROR unknown tool"),
        tool_result("", "CONTENTS OF secret.txt"),
    ];
    let out = translate(&history);
    let calls = blocks(&out.messages[1]);
    assert_eq!(calls.len(), 1, "the nameless call is dropped");
    assert_eq!(calls[0]["name"], "read_file");

    let results = blocks(&out.messages[2]);
    assert_eq!(results.len(), 1, "the nameless call's result is pruned");
    assert_eq!(
        results[0]["tool_use_id"], calls[0]["id"],
        "the surviving result must belong to the surviving call"
    );
    assert_eq!(
        results[0]["content"], "CONTENTS OF secret.txt",
        "read_file must not be told it returned the nameless call's output"
    );
}

#[test]
fn unclaimed_ids_from_an_earlier_turn_do_not_capture_a_later_result() {
    // Two assistant turns with empty-id calls where the first turn's results
    // were cut away. Turn 1's issued id must not be handed to turn 2's result.
    let history = vec![
        json!({"role": "user", "content": "go"}),
        assistant_tool_call("", "read_file", "{\"path\":\"first.txt\"}"),
        assistant_tool_call("", "read_file", "{\"path\":\"second.txt\"}"),
        tool_result("", "CONTENTS OF second.txt"),
    ];
    let out = translate(&history);
    // Turn 1 is pruned as an orphan; turn 2 keeps its own result.
    assert_eq!(roles(&out.messages), vec!["user", "assistant", "user"]);
    let call = &blocks(&out.messages[1])[0];
    assert_eq!(call["input"]["path"], "second.txt");
    assert_eq!(
        blocks(&out.messages[2])[0]["tool_use_id"],
        call["id"],
        "the result must bind to the turn it followed"
    );
}

// --- structural invariants ----------------------------------------------------

#[test]
fn empty_history_yields_one_user_turn() {
    let out = translate(&[]);
    assert_eq!(roles(&out.messages), vec!["user"]);
    assert_eq!(blocks(&out.messages[0])[0]["text"], CONTINUE_TEXT);
}

#[test]
fn system_only_history_yields_one_user_turn() {
    let out = translate(&[json!({"role": "system", "content": "prompt"})]);
    assert_eq!(out.system.as_deref(), Some("prompt"));
    assert_eq!(roles(&out.messages), vec!["user"]);
}

#[test]
fn assistant_first_history_gets_a_user_turn_prepended() {
    let history = vec![
        json!({"role": "system", "content": "prompt"}),
        json!({"role": "assistant", "content": "resumed mid-thought"}),
    ];
    let out = translate(&history);
    assert_eq!(roles(&out.messages), vec!["user", "assistant"]);
    assert_eq!(blocks(&out.messages[0])[0]["text"], CONTINUE_TEXT);
}

#[test]
fn consecutive_same_role_messages_are_left_alone() {
    // Legal on Anthropic: the API combines them into one turn.
    let history = vec![
        json!({"role": "user", "content": "one"}),
        json!({"role": "user", "content": "two"}),
    ];
    assert_eq!(roles(&translate(&history).messages), vec!["user", "user"]);
}

#[test]
fn a_dangling_tool_run_at_the_end_survives_translation() {
    // nudge_current_user_turn mutates an earlier user turn rather than
    // appending, so history routinely ends [assistant(tool_calls), tool, tool].
    let history = vec![
        json!({"role": "user", "content": "go"}),
        json!({
            "role": "assistant",
            "content": Value::Null,
            "tool_calls": [
                {"id": "c1", "type": "function", "function": {"name": "read_file", "arguments": "{}"}},
                {"id": "c2", "type": "function", "function": {"name": "read_file", "arguments": "{}"}},
            ],
        }),
        tool_result("c1", "a"),
        tool_result("c2", "b"),
    ];
    let out = translate(&history);
    assert_eq!(roles(&out.messages), vec!["user", "assistant", "user"]);
    assert_eq!(blocks(&out.messages[2]).len(), 2);
}
