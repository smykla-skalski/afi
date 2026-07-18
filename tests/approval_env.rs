//! Port of `tests/test_approval_env.py` - the env-state portion.
//!
//! Two tests from the Python file (`test_risk_classifier_retries_connection_failures`
//! and `test_open_stream_only_sets_sampler_params_for_recovery`) need the risk
//! classifier (phase 4) and the model stream (phase 5); they are deferred and
//! land with their implementations.

mod common;

use minion::approval::{normalize_approval, ApprovalKind, ApprovalState, Level};
use minion::banner;
use std::io::Write;

fn build(args: &[&str], env: &[(&str, &str)]) -> minion::Runtime {
    common::build(args, env)
}

// Default: prompts for everything.
#[test]
fn test_default_prompts_for_everything() {
    let rt = build(&["minion"], &[]);
    assert!(!rt.approval.yolo);
    assert_eq!(rt.approval.approve_level, None);
    assert_eq!(rt.approval.default_approve_level, None);
    assert!(banner(&rt).contains("prompt:all"));
}

// MINION_APPROVAL env var sets a persistent default.
#[test]
fn test_minion_approval_env_sets_persistent_default() {
    let rt = build(&["minion"], &[("MINION_APPROVAL", "medium")]);
    assert!(!rt.approval.yolo);
    assert_eq!(rt.approval.approve_level, Some(Level::Medium));
    assert_eq!(rt.approval.default_approve_level, Some(Level::Medium));
}

// MINION_APPROVAL can come from an env file (~/.env loader).
#[test]
fn test_minion_approval_can_come_from_env_file() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    writeln!(f, "MINION_APPROVAL=low").unwrap();
    let rt = common::build_with_env_file(&["minion"], &[], Some(f.path()));
    assert!(!rt.approval.yolo);
    assert_eq!(rt.approval.approve_level, Some(Level::Low));
    assert_eq!(rt.approval.default_approve_level, Some(Level::Low));
}

// --approval CLI flag overrides MINION_APPROVAL env (and yolo env).
#[test]
fn test_cli_approval_overrides_env_yolo() {
    let rt = build(
        &["minion", "--approval", "low"],
        &[("MINION_APPROVAL", "yolo")],
    );
    assert!(!rt.approval.yolo);
    assert_eq!(rt.approval.approve_level, Some(Level::Low));
    assert_eq!(rt.approval.default_approve_level, Some(Level::Low));
}

// --yolo overrides --approval level (but keeps the default the --approval set).
#[test]
fn test_cli_yolo_overrides_approval_level() {
    let rt = build(
        &["minion", "--approval", "medium", "--yolo"],
        &[("MINION_APPROVAL", "low")],
    );
    assert!(rt.approval.yolo);
    assert_eq!(rt.approval.approve_level, None);
    assert_eq!(rt.approval.default_approve_level, Some(Level::Medium));
}

// "all" / "strict" / "prompt" / "prompt-all" / "none" all mean prompt-all.
#[test]
fn test_prompt_all_aliases_are_accepted() {
    let rt = build(
        &["minion", "--approval", "all"],
        &[("MINION_APPROVAL", "medium")],
    );
    assert!(!rt.approval.yolo);
    assert_eq!(rt.approval.approve_level, None);
    assert_eq!(rt.approval.default_approve_level, None);
    assert_eq!(normalize_approval("strict"), Some(ApprovalKind::PromptAll));
    assert_eq!(normalize_approval("prompt"), Some(ApprovalKind::PromptAll));
    assert_eq!(
        normalize_approval("prompt-all"),
        Some(ApprovalKind::PromptAll)
    );
    assert_eq!(normalize_approval("none"), Some(ApprovalKind::PromptAll));
}

// A bad MINION_APPROVAL value doesn't crash; it leaves the default (prompt all).
#[test]
fn test_bad_minion_approval_falls_back_safely() {
    let rt = build(&["minion"], &[("MINION_APPROVAL", "nonsense")]);
    assert!(!rt.approval.yolo);
    assert_eq!(rt.approval.approve_level, None);
    // ApprovalState::default() is all-None.
    assert_eq!(rt.approval, ApprovalState::default());
}

// A bad --approval value doesn't crash; it keeps whatever came before.
#[test]
fn test_bad_cli_approval_falls_back_safely() {
    let rt = build(
        &["minion", "--approval", "nonsense"],
        &[("MINION_APPROVAL", "medium")],
    );
    // The env-applied medium survives the bad CLI flag.
    assert_eq!(rt.approval.approve_level, Some(Level::Medium));
    assert_eq!(rt.approval.default_approve_level, Some(Level::Medium));
}
