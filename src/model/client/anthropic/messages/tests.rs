//! Translator tests. Inputs are copied from the real producer shapes in
//! `turn_finalize`, `turn_dispatch`, `repl/core`, and `apply_compression` so
//! they stay honest about what afi actually puts in history.

use super::*;

fn roles(messages: &[Value]) -> Vec<&str> {
    messages
        .iter()
        .map(|m| m["role"].as_str().unwrap_or("?"))
        .collect()
}

fn blocks(message: &Value) -> &Vec<Value> {
    message["content"].as_array().expect("content is an array")
}

/// `turn_finalize.rs:166-174` - assistant with only tool calls.
fn assistant_tool_call(id: &str, name: &str, arguments: &str) -> Value {
    json!({
        "role": "assistant",
        "content": Value::Null,
        "tool_calls": [{
            "id": id,
            "type": "function",
            "function": {"name": name, "arguments": arguments},
        }],
    })
}

/// `turn_dispatch.rs:259` - one tool result per call.
fn tool_result(id: &str, content: &str) -> Value {
    json!({"role": "tool", "tool_call_id": id, "content": content})
}

// --- system -------------------------------------------------------------------

#[test]
fn system_is_hoisted_out_of_messages() {
    let history = vec![
        json!({"role": "system", "content": "You are a terminal coding agent."}),
        json!({"role": "user", "content": "hi"}),
    ];
    let out = translate(&history, Thinking::Drop);
    assert_eq!(
        out.system.as_deref(),
        Some("You are a terminal coding agent.")
    );
    assert_eq!(roles(&out.messages), vec!["user"]);
}

#[test]
fn multiple_system_messages_join() {
    let history = vec![
        json!({"role": "system", "content": "first"}),
        json!({"role": "user", "content": "hi"}),
        json!({"role": "system", "content": "second"}),
    ];
    let out = translate(&history, Thinking::Drop);
    assert_eq!(out.system.as_deref(), Some("first\n\nsecond"));
    assert_eq!(roles(&out.messages), vec!["user"]);
}

#[test]
fn no_system_message_means_no_system_field() {
    let out = translate(&[json!({"role": "user", "content": "hi"})], Thinking::Drop);
    assert!(out.system.is_none());
}

#[test]
fn blank_system_message_is_not_hoisted() {
    let history = vec![
        json!({"role": "system", "content": "   "}),
        json!({"role": "user", "content": "hi"}),
    ];
    assert!(translate(&history, Thinking::Drop).system.is_none());
}

// --- text ---------------------------------------------------------------------

#[test]
fn plain_strings_become_text_blocks() {
    let history = vec![
        json!({"role": "user", "content": "hello"}),
        json!({"role": "assistant", "content": "hi there"}),
    ];
    let out = translate(&history, Thinking::Drop);
    assert_eq!(roles(&out.messages), vec!["user", "assistant"]);
    assert_eq!(
        blocks(&out.messages[0])[0],
        json!({"type": "text", "text": "hello"})
    );
    assert_eq!(
        blocks(&out.messages[1])[0],
        json!({"type": "text", "text": "hi there"})
    );
}

#[test]
fn array_content_parts_are_flattened_to_text_blocks() {
    let history = vec![json!({
        "role": "user",
        "content": [{"type": "text", "text": "one"}, {"type": "text", "text": "two"}],
    })];
    let out = translate(&history, Thinking::Drop);
    assert_eq!(blocks(&out.messages[0]).len(), 2);
    assert_eq!(blocks(&out.messages[0])[1]["text"], "two");
}

#[test]
fn empty_and_blank_messages_are_dropped_entirely() {
    let history = vec![
        json!({"role": "user", "content": "real"}),
        json!({"role": "assistant", "content": ""}),
        json!({"role": "assistant", "content": "   "}),
        json!({"role": "assistant", "content": Value::Null}),
    ];
    // Anthropic rejects empty text blocks, and a message with no blocks at all.
    assert_eq!(
        roles(&translate(&history, Thinking::Drop).messages),
        vec!["user"]
    );
}

#[test]
fn non_text_content_parts_are_dropped() {
    let history = vec![json!({
        "role": "user",
        "content": [{"type": "image", "source": {}}, {"type": "text", "text": "kept"}],
    })];
    let out = translate(&history, Thinking::Drop);
    assert_eq!(blocks(&out.messages[0]).len(), 1);
    assert_eq!(blocks(&out.messages[0])[0]["text"], "kept");
}

#[test]
fn unknown_roles_are_dropped() {
    let history = vec![
        json!({"role": "user", "content": "hi"}),
        json!({"role": "function", "content": "legacy"}),
        json!({"content": "no role at all"}),
    ];
    assert_eq!(
        roles(&translate(&history, Thinking::Drop).messages),
        vec!["user"]
    );
}

// --- tool calls ---------------------------------------------------------------

#[test]
fn assistant_tool_call_becomes_tool_use_with_parsed_input() {
    let history = vec![
        json!({"role": "user", "content": "read it"}),
        assistant_tool_call("call_1", "read_file", r#"{"path":"a.rs"}"#),
        tool_result("call_1", "file contents"),
    ];
    let out = translate(&history, Thinking::Drop);
    assert_eq!(roles(&out.messages), vec!["user", "assistant", "user"]);
    // `arguments` was a JSON string; `input` must be a real object.
    assert_eq!(
        blocks(&out.messages[1])[0],
        json!({
            "type": "tool_use",
            "id": "call_1",
            "name": "read_file",
            "input": {"path": "a.rs"},
        })
    );
    assert_eq!(
        blocks(&out.messages[2])[0],
        json!({
            "type": "tool_result",
            "tool_use_id": "call_1",
            "content": "file contents",
        })
    );
}

#[test]
fn assistant_text_precedes_its_tool_use_blocks() {
    let history = vec![
        json!({"role": "user", "content": "go"}),
        json!({
            "role": "assistant",
            "content": "Let me look.",
            "tool_calls": [{
                "id": "c1", "type": "function",
                "function": {"name": "list_dir", "arguments": "{}"},
            }],
        }),
        tool_result("c1", "src/"),
    ];
    let out = translate(&history, Thinking::Drop);
    let assistant = blocks(&out.messages[1]);
    assert_eq!(assistant.len(), 2);
    assert_eq!(assistant[0]["type"], "text");
    assert_eq!(assistant[1]["type"], "tool_use");
}

#[test]
fn malformed_or_non_object_arguments_degrade_to_empty_input() {
    for arguments in ["", "{\"path\":", "null", "[]", "\"just a string\"", "7"] {
        let history = vec![
            json!({"role": "user", "content": "go"}),
            assistant_tool_call("c1", "read_file", arguments),
            tool_result("c1", "ok"),
        ];
        let out = translate(&history, Thinking::Drop);
        assert_eq!(
            blocks(&out.messages[1])[0]["input"],
            json!({}),
            "arguments {arguments:?} should degrade to an empty object"
        );
    }
}

#[test]
fn tool_calls_without_a_name_are_dropped() {
    let history = vec![
        json!({"role": "user", "content": "go"}),
        json!({
            "role": "assistant",
            "content": "text survives",
            "tool_calls": [{"id": "c1", "type": "function", "function": {"arguments": "{}"}}],
        }),
    ];
    let out = translate(&history, Thinking::Drop);
    let assistant = blocks(&out.messages[1]);
    assert_eq!(assistant.len(), 1);
    assert_eq!(assistant[0]["type"], "text");
}

#[test]
fn empty_tool_call_ids_get_matching_synthetic_ids() {
    // turn_dispatch writes tool_call_id via unwrap_or_default(), so a model that
    // omits ids produces "" on both sides. Anthropic needs them non-empty AND
    // matching.
    let history = vec![
        json!({"role": "user", "content": "go"}),
        assistant_tool_call("", "read_file", "{}"),
        tool_result("", "contents"),
    ];
    let out = translate(&history, Thinking::Drop);
    let call_id = blocks(&out.messages[1])[0]["id"].as_str().unwrap();
    let result_id = blocks(&out.messages[2])[0]["tool_use_id"].as_str().unwrap();
    assert!(!call_id.is_empty());
    assert_eq!(
        call_id, result_id,
        "synthetic ids must match across the turn"
    );
}

#[test]
fn empty_tool_result_content_gets_a_placeholder() {
    let history = vec![
        json!({"role": "user", "content": "go"}),
        assistant_tool_call("c1", "run_bash", "{}"),
        tool_result("c1", ""),
    ];
    let out = translate(&history, Thinking::Drop);
    assert_eq!(
        blocks(&out.messages[2])[0]["content"],
        EMPTY_TOOL_OUTPUT,
        "an empty tool result must not become an empty text block"
    );
}

// --- tool-result grouping -----------------------------------------------------

#[test]
fn consecutive_tool_results_collapse_into_one_user_message() {
    let history = vec![
        json!({"role": "user", "content": "go"}),
        json!({
            "role": "assistant",
            "content": Value::Null,
            "tool_calls": [
                {"id": "c1", "type": "function", "function": {"name": "read_file", "arguments": "{}"}},
                {"id": "c2", "type": "function", "function": {"name": "list_dir", "arguments": "{}"}},
            ],
        }),
        tool_result("c1", "one"),
        tool_result("c2", "two"),
    ];
    let out = translate(&history, Thinking::Drop);
    // Splitting results across messages trains the model out of parallel calls.
    assert_eq!(roles(&out.messages), vec!["user", "assistant", "user"]);
    let results = blocks(&out.messages[2]);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["tool_use_id"], "c1");
    assert_eq!(results[1]["tool_use_id"], "c2");
}

#[test]
fn the_esc_cancel_path_keeps_every_result() {
    // turn_dispatch pushes CANCELLED for the interrupted call and SKIPPED for
    // the rest, so the 1:1 tool_use/tool_result correspondence still holds.
    let history = vec![
        json!({"role": "user", "content": "go"}),
        json!({
            "role": "assistant",
            "content": Value::Null,
            "tool_calls": [
                {"id": "c1", "type": "function", "function": {"name": "run_bash", "arguments": "{}"}},
                {"id": "c2", "type": "function", "function": {"name": "run_bash", "arguments": "{}"}},
                {"id": "c3", "type": "function", "function": {"name": "run_bash", "arguments": "{}"}},
            ],
        }),
        tool_result("c1", "CANCELLED by user (Esc)"),
        tool_result("c2", "SKIPPED"),
        tool_result("c3", "SKIPPED"),
    ];
    let out = translate(&history, Thinking::Drop);
    assert_eq!(blocks(&out.messages[1]).len(), 3, "all tool_use kept");
    assert_eq!(blocks(&out.messages[2]).len(), 3, "all tool_result kept");
}

#[test]
fn tool_results_separated_by_a_user_turn_do_not_merge() {
    let history = vec![
        json!({"role": "user", "content": "go"}),
        assistant_tool_call("c1", "read_file", "{}"),
        tool_result("c1", "one"),
        json!({"role": "user", "content": "and again"}),
        assistant_tool_call("c2", "read_file", "{}"),
        tool_result("c2", "two"),
    ];
    let out = translate(&history, Thinking::Drop);
    assert_eq!(
        roles(&out.messages),
        vec!["user", "assistant", "user", "user", "assistant", "user"]
    );
}

mod prune;
