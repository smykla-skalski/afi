//! Level parsing, per-source translation, and what `EXTRA_BODY` keeps.

use super::*;
use crate::config::Protocol;
use serde_json::json;

fn source(base_url: &str, extra_body: Option<Value>) -> Source {
    Source::new("s", base_url.to_string(), None, None, extra_body, None)
}

fn anthropic(extra_body: Option<Value>) -> Source {
    source("https://api.anthropic.com", extra_body).with_protocol(Protocol::AnthropicApiKey)
}

// --- parsing ------------------------------------------------------------------

#[test]
fn every_level_round_trips_through_its_wire_name() {
    for name in LEVELS {
        let level = Effort::parse(name).expect("a listed level must parse");
        assert_eq!(level.as_str(), name);
    }
}

#[test]
fn case_and_surrounding_space_do_not_matter() {
    assert_eq!(Effort::parse(" XHigh "), Some(Effort::XHigh));
    assert_eq!(Effort::parse("MAX"), Some(Effort::Max));
}

#[test]
fn anything_else_is_refused_rather_than_rounded() {
    // Neighbouring spellings especially: they are what a typo looks like, and
    // silently picking the nearest level is the failure this flag exists to fix.
    for raw in ["", "  ", "highest", "x-high", "none", "9", "hgih"] {
        assert_eq!(Effort::parse(raw), None, "{raw:?} must not parse");
    }
}

// --- resolution ---------------------------------------------------------------

#[test]
fn nothing_configured_leaves_every_source_alone() {
    assert_eq!(resolve(None, None), Ok(None));
    assert_eq!(resolve(None, Some("   ")), Ok(None));
}

#[test]
fn the_flag_wins_over_the_variable() {
    assert_eq!(resolve(Some("low"), Some("max")), Ok(Some(Effort::Low)));
    assert_eq!(resolve(None, Some("max")), Ok(Some(Effort::Max)));
}

#[test]
fn a_bad_value_names_the_input_that_carried_it() {
    let flag = resolve(Some("hgih"), None).expect_err("a typo must not be ignored");
    assert!(flag.contains("--effort"), "{flag}");
    assert!(flag.contains("low|medium|high|xhigh|max"), "{flag}");

    let env = resolve(None, Some("hgih")).expect_err("a typo must not be ignored");
    assert!(env.contains("AFI_EFFORT"), "{env}");
}

// --- translation ---------------------------------------------------------------

#[test]
fn anthropic_carries_it_under_output_config() {
    let mut src = anthropic(None);
    apply(&mut src, Effort::High);
    assert_eq!(
        src.extra_body,
        Some(json!({"output_config": {"effort": "high"}}))
    );
    assert_eq!(src.resolved_effort(), Some("high"));
}

#[test]
fn openrouter_carries_it_under_reasoning() {
    let mut src = source("https://openrouter.ai/api/v1", None);
    apply(&mut src, Effort::Medium);
    assert_eq!(
        src.extra_body,
        Some(json!({"reasoning": {"effort": "medium"}}))
    );
    assert_eq!(src.resolved_effort(), Some("medium"));
}

#[test]
fn openai_carries_it_at_the_top_level() {
    let mut src = source("https://api.openai.com/v1", None);
    apply(&mut src, Effort::Low);
    assert_eq!(src.extra_body, Some(json!({"reasoning_effort": "low"})));
    assert_eq!(src.resolved_effort(), Some("low"));
}

#[test]
fn an_endpoint_with_no_known_dialect_is_left_untouched() {
    // llama.cpp, vLLM, Z.ai: guessing a key here would send one the endpoint
    // may reject, failing a turn over a preference.
    for base in [
        "http://localhost:8080/v1",
        "https://api.z.ai/api/paas/v4",
        "https://api.together.xyz/v1",
    ] {
        let mut src = source(base, None);
        apply(&mut src, Effort::Max);
        assert_eq!(src.extra_body, None, "{base} must carry nothing");
        assert_eq!(src.resolved_effort(), None, "{base} must report nothing");
    }
}

#[test]
fn a_level_above_the_dialects_ceiling_is_capped_rather_than_sent() {
    // `reasoning_effort` and `reasoning.effort` are defined up to `high`;
    // sending `max` would fail the turn instead of slowing it down.
    for base in ["https://openrouter.ai/api/v1", "https://api.openai.com/v1"] {
        for level in [Effort::XHigh, Effort::Max] {
            let mut src = source(base, None);
            apply(&mut src, level);
            assert_eq!(
                src.resolved_effort(),
                Some("high"),
                "{base} at {}",
                level.as_str()
            );
        }
    }
    // Anthropic defines the whole ladder, so nothing is capped there.
    let mut src = anthropic(None);
    apply(&mut src, Effort::Max);
    assert_eq!(src.resolved_effort(), Some("max"));
}

#[test]
fn the_note_says_what_the_source_ended_up_with() {
    let mut capped = source("https://api.openai.com/v1", None);
    apply(&mut capped, Effort::Max);
    let message = note(&capped, Effort::Max).expect("a capped level must be reported");
    assert!(message.contains("no effort above high"), "{message}");

    let mut overridden = anthropic(Some(json!({"output_config": {"effort": "low"}})));
    apply(&mut overridden, Effort::Max);
    let message = note(&overridden, Effort::Max).expect("an override must be reported");
    assert!(message.contains("already sets effort"), "{message}");

    let mut nowhere = source("http://localhost:8080/v1", None);
    apply(&mut nowhere, Effort::Max);
    let message = note(&nowhere, Effort::Max).expect("an untranslatable level must be reported");
    assert!(message.contains("nowhere to put it"), "{message}");
    // No advice to set a variable, since the source that most often raises this
    // (the legacy single-source fallback) reads no EXTRA_BODY at all.
    assert!(!message.contains("EXTRA_BODY"), "{message}");

    let mut sent = anthropic(None);
    apply(&mut sent, Effort::High);
    assert_eq!(note(&sent, Effort::High), None, "nothing to report");
}

#[test]
fn the_dialect_follows_the_endpoint_not_the_source_name() {
    // A source named anything at all, speaking the Messages API.
    let mut src = source("https://gateway.internal/anthropic/v1", None)
        .with_protocol(Protocol::AnthropicOAuth);
    apply(&mut src, Effort::XHigh);
    assert_eq!(
        src.extra_body,
        Some(json!({"output_config": {"effort": "xhigh"}}))
    );
}

// --- what EXTRA_BODY keeps ------------------------------------------------------

#[test]
fn a_hand_written_effort_wins() {
    let mut src = anthropic(Some(json!({"output_config": {"effort": "low"}})));
    apply(&mut src, Effort::Max);
    assert_eq!(src.resolved_effort(), Some("low"));

    let mut flat = source(
        "https://api.openai.com/v1",
        Some(json!({"reasoning_effort": "low"})),
    );
    apply(&mut flat, Effort::Max);
    assert_eq!(flat.resolved_effort(), Some("low"));
}

#[test]
fn a_container_that_is_already_there_is_left_as_written() {
    // Not merged into. OpenRouter documents `reasoning.effort` and
    // `reasoning.max_tokens` as mutually exclusive, so composing the two out of
    // a hand-written object would build a request neither side asked for - and
    // afi cannot know which keys any given endpoint pairs that way.
    let mut router = source(
        "https://openrouter.ai/api/v1",
        Some(json!({"reasoning": {"max_tokens": 2000}})),
    );
    apply(&mut router, Effort::High);
    assert_eq!(
        router.extra_body,
        Some(json!({"reasoning": {"max_tokens": 2000}}))
    );

    let mut src = anthropic(Some(json!({"output_config": {"task_budget": 64_000}})));
    apply(&mut src, Effort::High);
    assert_eq!(
        src.extra_body,
        Some(json!({"output_config": {"task_budget": 64_000}}))
    );
    // And it is said out loud, rather than the run quietly going without.
    let message = note(&src, Effort::High).expect("a skipped container must be reported");
    assert!(message.contains("output_config"), "{message}");
    assert!(message.contains("leaves as written"), "{message}");
}

#[test]
fn unrelated_keys_are_left_where_they_were() {
    let mut src = anthropic(Some(json!({"service_tier": "auto", "thinking": null})));
    apply(&mut src, Effort::High);
    assert_eq!(
        src.extra_body,
        Some(json!({
            "service_tier": "auto",
            "thinking": null,
            "output_config": {"effort": "high"},
        }))
    );
}

#[test]
fn a_container_of_the_wrong_type_is_not_rewritten() {
    // Whatever the caller meant by it, afi replacing it would lose it.
    let mut src = anthropic(Some(json!({"output_config": "high"})));
    apply(&mut src, Effort::Max);
    assert_eq!(src.extra_body, Some(json!({"output_config": "high"})));
    assert_eq!(src.resolved_effort(), None);
}

#[test]
fn a_flat_key_of_the_wrong_type_is_reported_rather_than_replaced() {
    // `reasoning_effort` is set but unreadable, so the run carries a level afi
    // cannot report - which is worth a line, not a silent overwrite.
    let mut src = source(
        "https://api.openai.com/v1",
        Some(json!({"reasoning_effort": 3})),
    );
    apply(&mut src, Effort::High);
    assert_eq!(src.extra_body, Some(json!({"reasoning_effort": 3})));
    let message = note(&src, Effort::High).expect("an unreadable level must be reported");
    assert!(message.contains("reasoning_effort"), "{message}");
}
