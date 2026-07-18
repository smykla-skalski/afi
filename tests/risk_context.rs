//! Port of `tests/test_risk_context.py` and the approval tests from
//! `tests/test_esc_approval.py` (the _confirm / _`ask_approval` portions).
//! The `model_turn` and REPL tests are deferred to phases 5 and 8.

use afi::approval::{ApprovalState, Level};
use afi::risk::{ApprovalChoice, RiskClassifier, confirm, extract_action_path};
use std::path::Path;

// --- Mock classifier ---

struct MockClassifier {
    level: Level,
    reason: String,
}

impl RiskClassifier for MockClassifier {
    fn classify(&self, _action: &str, _cwd: &Path, _project_root: &Path) -> (Level, String) {
        (self.level, self.reason.clone())
    }
}

// --- _confirm tests (from test_esc_approval.py) ---

#[test]
fn confirm_esc_raises() {
    let approval = ApprovalState::default();
    let classifier = MockClassifier {
        level: Level::High,
        reason: "because test".to_string(),
    };
    let result = confirm(
        "write foo.py (10 bytes)",
        &approval,
        &classifier,
        Path::new("."),
        Path::new("."),
        &|_| ApprovalChoice::Esc,
    );
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.0.contains("foo.py"));
}

#[test]
fn confirm_y_n() {
    let approval = ApprovalState::default();
    let classifier = MockClassifier {
        level: Level::High,
        reason: "because test".to_string(),
    };
    // Yes
    let result = confirm(
        "write foo.py",
        &approval,
        &classifier,
        Path::new("."),
        Path::new("."),
        &|_| ApprovalChoice::Yes,
    );
    assert!(result.unwrap());
    // No
    let result = confirm(
        "write foo.py",
        &approval,
        &classifier,
        Path::new("."),
        Path::new("."),
        &|_| ApprovalChoice::No,
    );
    assert!(!result.unwrap());
}

#[test]
fn confirm_yolo_short_circuits() {
    let approval = ApprovalState {
        yolo: true,
        ..ApprovalState::default()
    };
    let classifier = MockClassifier {
        level: Level::High,
        reason: "should not be called".to_string(),
    };
    let result = confirm(
        "write foo.py",
        &approval,
        &classifier,
        Path::new("."),
        Path::new("."),
        &|_| ApprovalChoice::No,
    );
    assert!(result.unwrap());
}

#[test]
fn confirm_auto_allows_below_threshold() {
    let approval = ApprovalState {
        yolo: false,
        approve_level: Some(Level::Medium),
        ..ApprovalState::default()
    };
    let classifier = MockClassifier {
        level: Level::Low,
        reason: "read-only".to_string(),
    };
    let result = confirm(
        "ls -la",
        &approval,
        &classifier,
        Path::new("."),
        Path::new("."),
        &|_| ApprovalChoice::No,
    );
    assert!(result.unwrap());
}

// --- _extract_action_path tests ---

#[test]
fn extract_edit_path() {
    assert_eq!(
        extract_action_path("edit src/main.rs"),
        Some("src/main.rs".to_string())
    );
}

#[test]
fn extract_write_path() {
    assert_eq!(
        extract_action_path("write src/main.rs (42 bytes)"),
        Some("src/main.rs".to_string())
    );
}

#[test]
fn extract_unknown_action() {
    assert_eq!(extract_action_path("run: echo hello"), None);
}
