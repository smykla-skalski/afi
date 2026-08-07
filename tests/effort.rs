//! `--effort` / `AFI_EFFORT` end to end: what each source ends up carrying,
//! what `EXTRA_BODY` keeps, and what refuses to start.

mod common;

use std::process::{Command, Output, Stdio};

use afi::summary::{ErrorKind, RunError};
use serde_json::json;
use tempfile::TempDir;

const LOCAL: (&str, &str) = ("AFI_SOURCE_LOCAL_BASE_URL", "http://localhost:8080/v1");
const ANTHROPIC_KEY: (&str, &str) = ("ANTHROPIC_API_KEY", "sk-ant-test");
const OPENROUTER_KEY: (&str, &str) = ("AFI_OPENROUTER_API_KEY", "sk-or-test");

// --- translation ----------------------------------------------------------------

#[test]
fn the_flag_reaches_every_source_in_its_own_spelling() {
    let rt = common::build(
        &["afi", "--effort", "high"],
        &[LOCAL, ANTHROPIC_KEY, OPENROUTER_KEY],
    );
    assert_eq!(
        rt.sources["anthropic"].extra_body,
        Some(json!({"output_config": {"effort": "high"}}))
    );
    assert_eq!(
        rt.sources["openrouter"].extra_body,
        Some(json!({
            "provider": {"order": ["parasail/fp8"], "allow_fallbacks": false},
            "reasoning": {"effort": "high"},
        })),
        "the built-in provider routing must survive the translation"
    );
    // llama.cpp has no equivalent afi knows, so nothing is guessed at it.
    assert_eq!(rt.sources["local"].extra_body, None);
}

#[test]
fn the_env_var_works_and_the_flag_beats_it() {
    let env = common::build(&["afi"], &[LOCAL, ANTHROPIC_KEY, ("AFI_EFFORT", "medium")]);
    assert_eq!(
        env.sources["anthropic"].extra_body,
        Some(json!({"output_config": {"effort": "medium"}}))
    );

    let flag = common::build(
        &["afi", "--effort", "low"],
        &[LOCAL, ANTHROPIC_KEY, ("AFI_EFFORT", "medium")],
    );
    assert_eq!(flag.sources["anthropic"].resolved_effort(), Some("low"));
}

#[test]
fn nothing_is_added_when_no_effort_is_asked_for() {
    let rt = common::build(&["afi"], &[LOCAL, ANTHROPIC_KEY, OPENROUTER_KEY]);
    assert_eq!(rt.sources["anthropic"].extra_body, None);
    assert_eq!(rt.sources["anthropic"].resolved_effort(), None);
    assert_eq!(
        rt.sources["openrouter"].extra_body,
        Some(json!({"provider": {"order": ["parasail/fp8"], "allow_fallbacks": false}}))
    );
}

#[test]
fn a_level_the_endpoint_does_not_define_is_capped_per_source() {
    // The same run, two ladders: Anthropic takes `max`, OpenRouter's unified
    // parameter stops at `high`, and neither turn fails over it.
    let rt = common::build(
        &["afi", "--effort", "max"],
        &[LOCAL, ANTHROPIC_KEY, OPENROUTER_KEY],
    );
    assert_eq!(rt.sources["anthropic"].resolved_effort(), Some("max"));
    assert_eq!(rt.sources["openrouter"].resolved_effort(), Some("high"));
}

// --- the escape hatch still wins --------------------------------------------------

#[test]
fn a_hand_written_effort_survives_the_flag() {
    let rt = common::build(
        &["afi", "--effort", "max"],
        &[
            LOCAL,
            ANTHROPIC_KEY,
            (
                "AFI_ANTHROPIC_EXTRA_BODY",
                r#"{"output_config":{"effort":"low"},"service_tier":"auto"}"#,
            ),
        ],
    );
    assert_eq!(
        rt.sources["anthropic"].extra_body,
        Some(json!({"output_config": {"effort": "low"}, "service_tier": "auto"}))
    );
    assert_eq!(rt.sources["anthropic"].resolved_effort(), Some("low"));
}

#[test]
fn a_hand_written_container_is_left_as_written() {
    // Merging into it would compose `reasoning: {"max_tokens": …, "effort": …}`,
    // two keys OpenRouter documents as mutually exclusive.
    let rt = common::build(
        &["afi", "--effort", "high"],
        &[
            LOCAL,
            OPENROUTER_KEY,
            (
                "AFI_SOURCE_OPENROUTER_EXTRA_BODY",
                r#"{"reasoning":{"max_tokens":2000}}"#,
            ),
        ],
    );
    assert_eq!(
        rt.sources["openrouter"].extra_body,
        Some(json!({"reasoning": {"max_tokens": 2000}}))
    );
    assert_eq!(rt.sources["openrouter"].resolved_effort(), None);
}

#[test]
fn another_flag_cannot_swallow_the_level() {
    // `--summary` used to consume `--effort` as its value and leave `xhigh` as a
    // stray positional: no summary, and a run at an effort nobody asked for.
    let rt = common::build(
        &["afi", "--summary", "--effort", "xhigh", "-f", "p.txt"],
        &[LOCAL, ANTHROPIC_KEY],
    );
    assert!(rt.refusals().is_empty(), "{:?}", rt.refusals());
    assert_eq!(rt.sources["anthropic"].resolved_effort(), Some("xhigh"));
    assert_eq!(rt.prompt_file.as_deref(), Some("p.txt"));
}

#[test]
fn an_effort_only_extra_body_is_still_reported() {
    // Nothing on the command line, so what the summary reports has to come from
    // the source itself.
    let rt = common::build(
        &["afi"],
        &[
            LOCAL,
            ANTHROPIC_KEY,
            (
                "AFI_ANTHROPIC_EXTRA_BODY",
                r#"{"output_config":{"effort":"xhigh"}}"#,
            ),
        ],
    );
    assert_eq!(rt.sources["anthropic"].resolved_effort(), Some("xhigh"));
}

// --- refusals ---------------------------------------------------------------------

#[test]
fn an_unknown_level_refuses_to_start() {
    for args in [
        vec!["afi", "--effort", "hgih"],
        vec!["afi", "--effort", "x-high"],
    ] {
        let rt = common::build(&args, &[LOCAL, ANTHROPIC_KEY]);
        let refusals = rt.refusals();
        assert_eq!(refusals.len(), 1, "{args:?} -> {refusals:?}");
        assert!(refusals[0].message.contains("--effort"), "{refusals:?}");
        // An effort this source has no answer for is the invocation, not a tool
        // policy - a caller retries the two differently.
        assert_eq!(refusals[0].kind, ErrorKind::Input, "{refusals:?}");
        assert_eq!(rt.sources["anthropic"].extra_body, None);
    }
}

#[test]
fn an_unknown_variable_refuses_too() {
    // Same failure mode as the flag: a typo in a YAML env block would otherwise
    // produce a complete run at an effort nobody asked for.
    let rt = common::build(&["afi"], &[LOCAL, ANTHROPIC_KEY, ("AFI_EFFORT", "highest")]);
    let refusals = rt.refusals();
    assert_eq!(refusals.len(), 1, "{refusals:?}");
    assert!(refusals[0].message.contains("AFI_EFFORT"), "{refusals:?}");
    assert_eq!(refusals[0].kind, ErrorKind::Input, "{refusals:?}");
}

#[test]
fn a_missing_value_refuses_rather_than_being_dropped() {
    let rt = common::build(&["afi", "--effort", "--yolo"], &[LOCAL, ANTHROPIC_KEY]);
    assert_eq!(
        rt.refusals(),
        vec![RunError::new("--effort needs a value", ErrorKind::Input)]
    );
    // Not consumed, so the flag after it still applies.
    assert!(rt.approval.yolo);
}

#[test]
fn a_blank_variable_is_simply_unset() {
    let rt = common::build(&["afi"], &[LOCAL, ANTHROPIC_KEY, ("AFI_EFFORT", "  ")]);
    assert!(rt.refusals().is_empty());
    // Neither refused nor read as a level - the run is the one it would have
    // been with the variable unexported.
    assert_eq!(rt.sources["anthropic"].extra_body, None);
}

/// Run the real binary with a clean env. A refused run exits before reading
/// stdin, so there is nothing to write to it.
fn run_afi(home: &TempDir, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_afi"))
        .args(args)
        .env_clear()
        .env("HOME", home.path())
        .env("AFI_HOME", home.path())
        .env("AFI_BASE_URL", "http://127.0.0.1:9/v1")
        .stdin(Stdio::null())
        .output()
        .expect("afi must start")
}

#[test]
fn the_process_exits_2_on_an_unusable_level() {
    let home = TempDir::new().unwrap();
    let output = run_afi(&home, &["--effort", "hgih"]);
    assert_eq!(output.status.code(), Some(2), "must not start");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("unknown --effort"), "{stderr}");
    assert!(stderr.contains("low|medium|high|xhigh|max"), "{stderr}");
    // The tool list answers a mistyped tool name; here it is noise, and the
    // levels the message does list are the answer.
    assert!(!stderr.contains("known tools:"), "{stderr}");
}
