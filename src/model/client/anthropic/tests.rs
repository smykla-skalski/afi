use super::*;
use crate::model::{FINAL_ANSWER_TOOL_CHOICE, TURN_DONE};
use crate::tools::TOOLS;

fn history() -> Vec<Value> {
    vec![
        json!({"role": "system", "content": "You are a terminal coding agent."}),
        json!({"role": "user", "content": "hello"}),
    ]
}

fn body_with(extra_body: Option<&Value>) -> Value {
    build_body(&BodyParams {
        model: "claude-sonnet-5",
        history: &history(),
        tools: Some(&TOOLS),
        tool_choice: None,
        max_tokens: Some(16_000),
        extra_body,
        stream: true,
    })
}

// --- url normalization --------------------------------------------------------

#[test]
fn messages_url_tolerates_a_version_suffix() {
    // Sources are conventionally configured with /v1, so it must not double up.
    for base in [
        "https://api.anthropic.com",
        "https://api.anthropic.com/",
        "https://api.anthropic.com/v1",
        "https://api.anthropic.com/v1/",
    ] {
        assert_eq!(
            messages_url(base),
            "https://api.anthropic.com/v1/messages",
            "base {base} normalized wrong"
        );
    }
}

#[test]
fn token_url_shares_the_same_normalization() {
    assert_eq!(
        token_url("https://api.anthropic.com/v1"),
        "https://api.anthropic.com/v1/oauth/token"
    );
}

#[test]
fn a_gateway_path_is_preserved() {
    assert_eq!(
        messages_url("https://gateway.internal/anthropic/v1"),
        "https://gateway.internal/anthropic/v1/messages"
    );
}

// --- required fields ----------------------------------------------------------

#[test]
fn max_tokens_is_always_present_and_floored() {
    // Anthropic requires max_tokens; the forced-final path asks for 2048, which
    // adaptive thinking can consume entirely.
    for (requested, expected) in [
        (None, DEFAULT_MAX_TOKENS),
        (Some(0), DEFAULT_MAX_TOKENS),
        (Some(2048), MIN_MAX_TOKENS),
        (Some(64_000), 64_000),
    ] {
        assert_eq!(clamp_max_tokens(requested), expected);
    }
    assert_eq!(body_with(None)["max_tokens"], 16_000);
}

#[test]
fn stream_flag_is_explicit() {
    assert_eq!(body_with(None)["stream"], true);
}

#[test]
fn thinking_is_explicitly_disabled_and_cannot_be_turned_on() {
    // A thinking block must be echoed back verbatim alongside its tool_use on
    // the follow-up request, which afi cannot do while history lives in OpenAI
    // shape. Disabled must also be explicit, since thinking is on by default on
    // Opus 5, Sonnet 5, and Fable 5.
    assert_eq!(body_with(None)["thinking"], json!({"type": "disabled"}));

    // EXTRA_BODY must not be able to turn it back on.
    let opt_in = json!({"thinking": {"type": "adaptive"}, "output_config": {"effort": "high"}});
    let body = body_with(Some(&opt_in));
    assert_eq!(
        body["thinking"],
        json!({"type": "disabled"}),
        "thinking is not allowlisted, so it stays off"
    );
    assert_eq!(body["output_config"], json!({"effort": "high"}));
}

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

#[test]
fn tools_are_unwrapped_and_tool_choice_translated() {
    let body = build_body(&BodyParams {
        model: "claude-sonnet-5",
        history: &history(),
        tools: Some(&TOOLS),
        tool_choice: Some(&FINAL_ANSWER_TOOL_CHOICE),
        max_tokens: None,
        extra_body: None,
        stream: true,
    });
    let tools = body["tools"].as_array().unwrap();
    assert!(tools[0].get("input_schema").is_some());
    assert!(tools[0].get("function").is_none());
    assert_eq!(
        body["tool_choice"],
        json!({"type": "tool", "name": "final_answer"})
    );
}

// --- sampler suppression ------------------------------------------------------

#[test]
fn sampling_parameters_never_reach_the_wire() {
    // The exact shape recovery_sampling_opts produces, merged into extra_body.
    let recovery = json!({
        "temperature": 0.7,
        "top_p": 0.95,
        "extra_body": {
            "min_p": 0.05,
            "repeat_penalty": 1.1,
            "repeat_last_n": 256,
            "dry_multiplier": 0.8,
        },
        "provider": {"order": ["parasail/fp8"]},
    });
    let body = body_with(Some(&recovery));
    for rejected in [
        "temperature",
        "top_p",
        "top_k",
        "extra_body",
        "stream_options",
        "provider",
        "min_p",
        "repeat_penalty",
    ] {
        assert!(
            body.get(rejected).is_none(),
            "{rejected} must not be sent to Anthropic"
        );
    }
}

#[test]
fn allowlisted_extras_are_applied() {
    let configured = json!({
        "output_config": {"effort": "high"},
        "stop_sequences": ["STOP"],
        "metadata": {"user_id": "u1"},
        "temperature": 0.9,
    });
    let body = body_with(Some(&configured));
    assert_eq!(body["output_config"], json!({"effort": "high"}));
    assert_eq!(body["stop_sequences"], json!(["STOP"]));
    assert_eq!(body["metadata"], json!({"user_id": "u1"}));
    assert!(body.get("temperature").is_none(), "still not allowlisted");
}

#[test]
fn a_non_object_extra_body_is_ignored() {
    let body = body_with(Some(&json!("nonsense")));
    assert_eq!(body["thinking"], json!({"type": "disabled"}));
}

#[test]
fn a_wrapped_extra_body_applies_nothing() {
    // The shape the removed `Source::extra_request_kwargs()` produced - a Python
    // OpenAI SDK kwarg the Rust client never unwrapped. Callers must pass the
    // source's own keys directly; anything nested under `extra_body` is inert,
    // which is why `/compress` silently dropped configuration on every source.
    let wrapped = json!({"extra_body": {"output_config": {"effort": "low"}}});
    let body = body_with(Some(&wrapped));
    assert!(
        body.get("output_config").is_none(),
        "nested config must not be silently half-applied"
    );
    assert!(body.get("extra_body").is_none());
}

// --- response reshaping -------------------------------------------------------

#[test]
fn completion_is_reshaped_into_openai_form() {
    let anthropic = r#"{
        "id": "msg_1", "type": "message", "role": "assistant",
        "content": [
            {"type": "thinking", "thinking": ""},
            {"type": "text", "text": "first "},
            {"type": "text", "text": "second"}
        ],
        "stop_reason": "end_turn"
    }"#;
    let reshaped: Value = serde_json::from_str(&reshape_completion(anthropic).unwrap()).unwrap();
    // This is exactly what repl/commands.rs parses, so it needs no branch.
    assert_eq!(reshaped["choices"][0]["message"]["content"], "first second");
}

#[test]
fn a_text_free_response_is_an_error_not_an_empty_summary() {
    // The only caller feeds this into apply_compression, which replaces the
    // conversation with the summary and has no empty-summary guard. Returning
    // Ok("") here would wipe history while reporting success.
    for body in [
        "not json",
        "{}",
        r#"{"content":[]}"#,
        r#"{"content":"str"}"#,
        r#"{"content":[{"type":"thinking","thinking":"..."}]}"#,
        r#"{"content":[{"type":"text","text":"   "}]}"#,
    ] {
        assert!(
            reshape_completion(body).is_err(),
            "{body} must not reshape into a successful empty summary"
        );
    }
}

#[test]
fn a_text_free_response_names_its_stop_reason() {
    // A pre-output refusal is a 200 with an empty content array, so the stop
    // reason is the only clue about what happened.
    let refusal = r#"{"content":[],"stop_reason":"refusal"}"#;
    let err = reshape_completion(refusal).unwrap_err().to_string();
    assert!(err.contains("refusal"), "got {err}");

    let truncated =
        r#"{"content":[{"type":"thinking","thinking":"x"}],"stop_reason":"max_tokens"}"#;
    let err = reshape_completion(truncated).unwrap_err().to_string();
    assert!(err.contains("max_tokens"), "got {err}");
}

#[test]
fn turn_status_constants_are_untouched_by_this_protocol() {
    // Guards against accidentally coupling protocol work to the turn vocabulary.
    assert_eq!(TURN_DONE, "done");
}

// --- non-streaming usage accounting ------------------------------------------

use super::completion_usage;

/// A `/compress` response body carrying the usage counts Anthropic returns.
fn completion_body(input: u64, cache_read: u64, cache_creation: u64, output: u64) -> String {
    format!(
        r#"{{"content":[{{"type":"text","text":"summary"}}],"usage":{{"input_tokens":{input},"cache_read_input_tokens":{cache_read},"cache_creation_input_tokens":{cache_creation},"output_tokens":{output}}}}}"#
    )
}

#[test]
fn a_compression_request_reports_its_cache_write_separately() {
    // /compress never reaches the streaming path, so it normalizes here. A
    // write folded into input on this path would under-price it exactly as it
    // did on the streamed one.
    let usage = completion_usage(&completion_body(120, 0, 2279, 64)).expect("usage present");
    assert_eq!(
        (
            usage.input_tokens,
            usage.cache_read_tokens,
            usage.cache_write_tokens,
            usage.output_tokens
        ),
        (120, 0, 2279, 64)
    );
}

#[test]
fn a_compression_request_against_a_warm_cache_reports_no_write() {
    let usage = completion_usage(&completion_body(40, 6837, 0, 51)).expect("usage present");
    assert_eq!(
        (
            usage.input_tokens,
            usage.cache_read_tokens,
            usage.cache_write_tokens
        ),
        (40, 6837, 0)
    );
}

#[test]
fn a_response_without_usage_is_not_counted() {
    // Best effort: an unparseable or usage-free body must not record zeros,
    // which would make a silent response look like a free request.
    assert!(completion_usage(r#"{"content":[]}"#).is_none());
    assert!(completion_usage("not json").is_none());
}
