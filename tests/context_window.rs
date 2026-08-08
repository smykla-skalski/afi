//! Where a run's context window comes from, and in what order.
//!
//! The auto-compress threshold is a percentage of the window, so the window is
//! what decides whether a session folds at all. It is resolved from four places
//! and they disagree on purpose: a flag typed for this run, a variable set for
//! this source, a variable set for every source, and a table compiled into the
//! binary. Each test below pins one step of that order against the next, because
//! a precedence nobody checks is one that quietly inverts.
//!
//! Read off `Runtime` rather than from a live run: the resolution happens when a
//! source becomes active, and every case here is about configuration rather than
//! about the wire. What happens once a window *is* known is in `autocompress.rs`.

mod common;

use common::build;

/// The window `Runtime` resolved for the source it started on.
fn window_of(args: &[&str], env: &[(&str, &str)]) -> Option<u64> {
    let rt = build(args, env);
    let active = rt.active.clone().expect("a run must start on a source");
    rt.sources[&active].context_window
}

/// The env every case starts from: one source, on a model no compiled row knows,
/// so nothing resolves unless the case under test says so.
fn unknown_model() -> Vec<(&'static str, &'static str)> {
    vec![
        ("AFI_BASE_URL", "http://localhost:8080/v1"),
        ("AFI_MODEL", "some-local-gguf"),
    ]
}

#[test]
fn an_unknown_model_leaves_the_window_unknown() {
    // The llama.cpp case, and the one the issue names: nothing to measure a
    // threshold against, so the run must not invent one.
    assert_eq!(window_of(&["afi"], &unknown_model()), None);
}

#[test]
fn a_known_model_resolves_from_the_compiled_table() {
    let env = [
        ("AFI_BASE_URL", "https://api.z.ai/api/paas/v4"),
        ("AFI_MODEL", "glm-4.6"),
    ];
    assert_eq!(window_of(&["afi"], &env), Some(204_800));
}

#[test]
fn the_model_a_source_switches_to_is_the_one_that_resolves() {
    // `/source zai glm-4.6` pins a model the source never named, and the window
    // belongs to the model rather than to the endpoint.
    let env = [
        ("AFI_SOURCE_ZAI_BASE_URL", "https://api.z.ai/api/paas/v4"),
        ("AFI_SOURCE_ZAI_MODEL", "glm-5.2"),
    ];
    let mut rt = build(&["afi"], &env);
    assert_eq!(rt.sources["zai"].context_window, Some(1_000_000));
    assert!(rt.switch_source("zai", Some("glm-4.6")));
    assert_eq!(rt.sources["zai"].context_window, Some(204_800));
}

#[test]
fn a_run_wide_variable_answers_for_a_source_that_declares_nothing() {
    let mut env = unknown_model();
    env.push(("AFI_CONTEXT_WINDOW", "65536"));
    assert_eq!(window_of(&["afi"], &env), Some(65536));
}

#[test]
fn a_declared_window_wins_over_the_table() {
    // The table is a fallback, not an authority: a server started with `-c 32768`
    // holds what it was started with, whatever the weights can do.
    let env = [
        ("AFI_SOURCE_ZAI_BASE_URL", "https://api.z.ai/api/paas/v4"),
        ("AFI_SOURCE_ZAI_MODEL", "glm-4.6"),
        ("AFI_SOURCE_ZAI_CONTEXT_WINDOW", "32768"),
    ];
    assert_eq!(window_of(&["afi"], &env), Some(32768));
}

#[test]
fn the_source_variable_wins_over_the_run_wide_one() {
    let mut env = unknown_model();
    env.push(("AFI_CONTEXT_WINDOW", "65536"));
    env.push(("AFI_LOCAL_CONTEXT_WINDOW", "32768"));
    assert_eq!(window_of(&["afi"], &env), Some(32768));
}

#[test]
fn the_built_in_namespace_answers_for_the_built_in_source() {
    // The `anthropic` source takes its model and base url from `AFI_ANTHROPIC_*`
    // rather than from the `AFI_SOURCE_*` namespace, and its window comes from
    // the same place.
    let env = [
        ("ANTHROPIC_API_KEY", "sk-test"),
        ("AFI_ACTIVE", "anthropic"),
        ("AFI_ANTHROPIC_CONTEXT_WINDOW", "123456"),
    ];
    assert_eq!(window_of(&["afi"], &env), Some(123_456));
}

#[test]
fn the_flag_wins_over_every_configured_value() {
    let env = [
        ("AFI_SOURCE_ZAI_BASE_URL", "https://api.z.ai/api/paas/v4"),
        ("AFI_SOURCE_ZAI_MODEL", "glm-4.6"),
        ("AFI_SOURCE_ZAI_CONTEXT_WINDOW", "32768"),
        ("AFI_CONTEXT_WINDOW", "65536"),
    ];
    assert_eq!(
        window_of(&["afi", "--context-window", "4096"], &env),
        Some(4096)
    );
}

#[test]
fn a_window_declared_as_zero_is_an_answer_rather_than_a_gap() {
    // Zero is how an operator turns folding off for one source without touching
    // the percentage, so it must not fall through to the table underneath it.
    let env = [
        ("AFI_SOURCE_ZAI_BASE_URL", "https://api.z.ai/api/paas/v4"),
        ("AFI_SOURCE_ZAI_MODEL", "glm-4.6"),
        ("AFI_SOURCE_ZAI_CONTEXT_WINDOW", "0"),
    ];
    assert_eq!(window_of(&["afi"], &env), Some(0));
}

#[test]
fn an_unreadable_variable_falls_through_to_the_next_spelling() {
    // Every other `AFI_*` number behaves this way, and the fallback here is a
    // window rather than a wrong one: the run reaches the table, or reaches
    // nothing and says so.
    let env = [
        ("AFI_SOURCE_ZAI_BASE_URL", "https://api.z.ai/api/paas/v4"),
        ("AFI_SOURCE_ZAI_MODEL", "glm-4.6"),
        ("AFI_SOURCE_ZAI_CONTEXT_WINDOW", "lots"),
    ];
    assert_eq!(window_of(&["afi"], &env), Some(204_800));
}

#[test]
fn an_unreadable_flag_refuses_to_start() {
    // The flag has nowhere to fall through to: it was typed for this run, and a
    // run that ignores it measures its threshold against a different number than
    // the command line asked for.
    let rt = build(&["afi", "--context-window", "lots"], &unknown_model());
    let refusals = rt.refusals();
    assert!(
        refusals
            .iter()
            .any(|error| error.message.contains("--context-window")),
        "the run must name the flag it cannot read: {refusals:?}"
    );
}

#[test]
fn the_flag_needs_a_value() {
    let rt = build(&["afi", "--context-window"], &unknown_model());
    assert!(
        rt.refusals()
            .iter()
            .any(|error| error.message.contains("--context-window")),
        "a flag with no value must refuse to start"
    );
}
