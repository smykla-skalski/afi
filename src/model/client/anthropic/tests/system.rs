//! What reaches the Messages API as `system`, and how it is cached.
//!
//! Split out of the parent module so the two halves - request shape and
//! system content - stay under the source-size cap independently.

use super::*;

#[test]
fn system_is_hoisted_and_marked_cacheable() {
    let body = body_with(None);
    let system = body["system"].as_array().expect("system is a block array");
    assert_eq!(system.len(), 1);
    assert_eq!(system[0]["text"], "You are a terminal coding agent.");
    assert_eq!(system[0]["cache_control"], json!({"type": "ephemeral"}));
    // It must not remain in messages.
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["role"], "user");
}

#[test]
fn a_supplied_prompt_reaches_the_messages_api_as_system_content() {
    // The Anthropic half of "a configured prompt is system content, not a user
    // message". The prompt is composed before the request is built, so what has
    // to be proved here is that the composed text arrives whole and stays one
    // cacheable block - the breakpoint is what makes a multi-turn run affordable,
    // and it sits on the last system block whatever that block says.
    let supplied = "Review the diff. Report APPROVE or REQUEST_CHANGES.";
    let body = build_body(&BodyParams {
        model: "claude-sonnet-5",
        history: &[
            json!({"role": "system", "content": supplied}),
            json!({"role": "user", "content": "hello"}),
        ],
        tools: Some(&TOOLS),
        tool_choice: None,
        max_tokens: Some(16_000),
        extra_body: None,
        stream: true,
    });
    let system = body["system"].as_array().expect("system is a block array");
    assert_eq!(system.len(), 1, "one block, so one breakpoint covers it");
    assert_eq!(system[0]["text"], supplied);
    assert_eq!(system[0]["cache_control"], json!({"type": "ephemeral"}));
}

#[test]
fn the_built_in_prompt_still_renders_the_same_cache_prefix() {
    // A run that configures nothing has to keep hitting the cache it filled
    // before this setting existed, which means the exact bytes of the block.
    let body = build_body(&BodyParams {
        model: "claude-sonnet-5",
        history: &[
            json!({"role": "system", "content": prompt::system()}),
            json!({"role": "user", "content": "hello"}),
        ],
        tools: Some(&TOOLS),
        tool_choice: None,
        max_tokens: Some(16_000),
        extra_body: None,
        stream: true,
    });
    assert_eq!(body["system"][0]["text"], prompt::system());
}

#[test]
fn no_system_message_means_no_system_key() {
    let body = build_body(&BodyParams {
        model: "claude-sonnet-5",
        history: &[json!({"role": "user", "content": "hi"})],
        tools: None,
        tool_choice: None,
        max_tokens: None,
        extra_body: None,
        stream: false,
    });
    assert!(body.get("system").is_none());
    assert_eq!(body["stream"], false);
}
