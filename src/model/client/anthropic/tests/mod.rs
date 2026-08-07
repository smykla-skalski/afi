use super::*;
use crate::model::{FINAL_ANSWER_TOOL_CHOICE, TURN_DONE};
use crate::prompt;
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
        assert_eq!(
            clamp_max_tokens(requested, thinking::Thinking::Drop),
            expected
        );
    }
    assert_eq!(body_with(None)["max_tokens"], 16_000);
}

#[test]
fn the_floor_rises_once_the_request_may_think() {
    // 4096 is room for an answer and not for the reasoning in front of it: the
    // forced-final turn spends the lot thinking and comes back empty.
    for (requested, expected) in [
        (None, DEFAULT_MAX_TOKENS),
        (Some(2048), MIN_MAX_TOKENS_THINKING),
        (Some(4096), MIN_MAX_TOKENS_THINKING),
        // Never downward: a caller who asked for more keeps it.
        (Some(64_000), 64_000),
    ] {
        assert_eq!(
            clamp_max_tokens(requested, thinking::Thinking::Replay),
            expected
        );
    }
}

#[test]
fn a_forced_final_at_max_effort_is_not_left_at_the_disabled_floor() {
    // The whole sequence: effort above `high` omits the `disabled` default,
    // which turns thinking on, which is what makes 4096 too small.
    let body = build_body(&BodyParams {
        model: "claude-opus-5",
        history: &history(),
        tools: Some(&TOOLS),
        tool_choice: None,
        max_tokens: Some(2048),
        extra_body: Some(&json!({"output_config": {"effort": "max"}})),
        stream: true,
    });
    assert_eq!(body.get("thinking"), None, "max effort rejects disabled");
    assert_eq!(body["max_tokens"], MIN_MAX_TOKENS_THINKING);
}

#[test]
fn stream_flag_is_explicit() {
    assert_eq!(body_with(None)["stream"], true);
}

#[test]
fn thinking_defaults_to_an_explicit_disabled() {
    // Explicit rather than omitted: thinking is on by default on Opus 5,
    // Sonnet 5, and Fable 5, and Haiku 4.5 rejects adaptive outright.
    assert_eq!(body_with(None)["thinking"], json!({"type": "disabled"}));
}

#[test]
fn extra_body_can_turn_thinking_on() {
    let opt_in = json!({"thinking": {"type": "adaptive", "display": "summarized"},
                        "output_config": {"effort": "high"}});
    let body = body_with(Some(&opt_in));
    assert_eq!(
        body["thinking"],
        json!({"type": "adaptive", "display": "summarized"})
    );
    assert_eq!(body["output_config"], json!({"effort": "high"}));
}

#[test]
fn a_null_thinking_omits_the_key() {
    // Claude Fable 5 rejects an explicit `disabled` and always thinks, so
    // omission is the only shape it accepts.
    let body = body_with(Some(&json!({"thinking": null})));
    assert!(
        body.get("thinking").is_none(),
        "thinking must be absent, not null"
    );
}

#[test]
fn thinking_blocks_replay_only_when_thinking_is_on() {
    let block = json!({"type": "thinking", "thinking": "plan", "signature": "sig"});
    let history = vec![
        json!({"role": "user", "content": "read it"}),
        json!({
            "role": "assistant",
            "content": Value::Null,
            "tool_calls": [{"id": "call_1", "type": "function",
                            "function": {"name": "read_file", "arguments": "{}"}}],
            thinking::THINKING_HISTORY_KEY: [block],
        }),
        json!({"role": "tool", "tool_call_id": "call_1", "content": "contents"}),
    ];
    let body = |extra_body: Option<&Value>| {
        build_body(&BodyParams {
            model: "claude-sonnet-5",
            history: &history,
            tools: None,
            tool_choice: None,
            max_tokens: None,
            extra_body,
            stream: true,
        })
    };

    // Default (disabled): the blocks are dropped, matching what the request asks for.
    let off = body(None);
    assert_eq!(off["messages"][1]["content"][0]["type"], "tool_use");

    // Adaptive: the block comes back verbatim, ahead of the tool_use it belongs to.
    let on = body(Some(&json!({"thinking": {"type": "adaptive"}})));
    let blocks = on["messages"][1]["content"].as_array().unwrap();
    assert_eq!(blocks[0], block);
    assert_eq!(blocks[1]["type"], "tool_use");
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

mod system;
