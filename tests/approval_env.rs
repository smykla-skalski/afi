//! Port of `tests/test_approval_env.py` - the env-state portion.
//!
//! Two tests from the Python file (`test_risk_classifier_retries_connection_failures`
//! and `test_open_stream_only_sets_sampler_params_for_recovery`) need the risk
//! classifier (phase 4) and the model stream (phase 5); they are deferred and
//! land with their implementations.

mod common;

use afi::approval::{ApprovalKind, ApprovalState, Level, normalize_approval};
use afi::banner;
use std::io::Write;

fn build(args: &[&str], env: &[(&str, &str)]) -> afi::Runtime {
    common::build(args, env)
}

// Default: prompts for everything.
#[test]
fn test_default_prompts_for_everything() {
    let rt = build(&["afi"], &[]);
    assert!(!rt.approval.yolo);
    assert_eq!(rt.approval.approve_level, None);
    assert_eq!(rt.approval.default_approve_level, None);
    assert!(banner(&rt).contains("prompt:all"));
}

// AFI_APPROVAL env var sets a persistent default.
#[test]
fn test_afi_approval_env_sets_persistent_default() {
    let rt = build(&["afi"], &[("AFI_APPROVAL", "medium")]);
    assert!(!rt.approval.yolo);
    assert_eq!(rt.approval.approve_level, Some(Level::Medium));
    assert_eq!(rt.approval.default_approve_level, Some(Level::Medium));
}

// AFI_APPROVAL can come from an env file (~/.env loader).
#[test]
fn test_afi_approval_can_come_from_env_file() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    writeln!(f, "AFI_APPROVAL=low").unwrap();
    let rt = common::build_with_env_file(&["afi"], &[], Some(f.path()));
    assert!(!rt.approval.yolo);
    assert_eq!(rt.approval.approve_level, Some(Level::Low));
    assert_eq!(rt.approval.default_approve_level, Some(Level::Low));
}

// --approval CLI flag overrides AFI_APPROVAL env (and yolo env).
#[test]
fn test_cli_approval_overrides_env_yolo() {
    let rt = build(&["afi", "--approval", "low"], &[("AFI_APPROVAL", "yolo")]);
    assert!(!rt.approval.yolo);
    assert_eq!(rt.approval.approve_level, Some(Level::Low));
    assert_eq!(rt.approval.default_approve_level, Some(Level::Low));
}

// --yolo overrides --approval level (but keeps the default the --approval set).
#[test]
fn test_cli_yolo_overrides_approval_level() {
    let rt = build(
        &["afi", "--approval", "medium", "--yolo"],
        &[("AFI_APPROVAL", "low")],
    );
    assert!(rt.approval.yolo);
    assert_eq!(rt.approval.approve_level, None);
    assert_eq!(rt.approval.default_approve_level, Some(Level::Medium));
}

// `--approval all` clears any env-set level back to prompt-all.
#[test]
fn test_prompt_all_flag_clears_level() {
    let rt = build(&["afi", "--approval", "all"], &[("AFI_APPROVAL", "medium")]);
    assert!(!rt.approval.yolo);
    assert_eq!(rt.approval.approve_level, None);
    assert_eq!(rt.approval.default_approve_level, None);
}

// "all" / "strict" / "prompt" / "prompt-all" / "none" all mean prompt-all.
#[test]
fn test_prompt_all_aliases_normalize() {
    assert_eq!(normalize_approval("strict"), Some(ApprovalKind::PromptAll));
    assert_eq!(normalize_approval("prompt"), Some(ApprovalKind::PromptAll));
    assert_eq!(
        normalize_approval("prompt-all"),
        Some(ApprovalKind::PromptAll)
    );
    assert_eq!(normalize_approval("none"), Some(ApprovalKind::PromptAll));
}

// A bad AFI_APPROVAL value doesn't crash; it leaves the default (prompt all).
#[test]
fn test_bad_afi_approval_falls_back_safely() {
    let rt = build(&["afi"], &[("AFI_APPROVAL", "nonsense")]);
    assert!(!rt.approval.yolo);
    assert_eq!(rt.approval.approve_level, None);
    // ApprovalState::default() is all-None.
    assert_eq!(rt.approval, ApprovalState::default());
}

// A bad --approval value doesn't crash; it keeps whatever came before.
#[test]
fn test_bad_cli_approval_falls_back_safely() {
    let rt = build(
        &["afi", "--approval", "nonsense"],
        &[("AFI_APPROVAL", "medium")],
    );
    // The env-applied medium survives the bad CLI flag.
    assert_eq!(rt.approval.approve_level, Some(Level::Medium));
    assert_eq!(rt.approval.default_approve_level, Some(Level::Medium));
}
