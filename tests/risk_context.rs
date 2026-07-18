//! Port of `tests/test_risk_context.py` and the approval tests from
//! `tests/test_esc_approval.py` (the _confirm / _ask_approval portions).
//! The model_turn and REPL tests are deferred to phases 5 and 8.

use minion::approval::{ApprovalState, Level};
use minion::risk::{
    confirm, extract_action_path, risk_user_message, ApprovalChoice, RiskClassifier,
};
use std::fs;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use tempfile::tempdir;

/// Serialize tests that set HOME (env vars aren't thread-safe).
fn home_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

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

// --- risk_context tests (from test_risk_context.py) ---

#[test]
fn downloads_project_file_is_in_project() {
    let _guard = home_lock().lock().unwrap();
    let home = tempdir().unwrap();
    std::env::set_var("HOME", home.path());
    let project = home.path().join("Downloads").join("didenstuff");
    fs::create_dir_all(&project).unwrap();
    let target = project.join("pose_editor.py");
    fs::write(&target, "").unwrap();
    let cwd = project.canonicalize().unwrap();
    let project_root = cwd.clone();
    let target_abs = target.canonicalize().unwrap();

    let msg = risk_user_message(&format!("edit {}", target.display()), &cwd, &project_root);
    let payload: serde_json::Value = serde_json::from_str(&msg).unwrap();
    assert_eq!(
        payload["project_root"],
        project_root.to_string_lossy().to_string()
    );
    assert_eq!(
        payload["primary_path"],
        target_abs.to_string_lossy().to_string()
    );
    assert_eq!(payload["path_scope"], "in_project");
    assert_eq!(payload["path_in_downloads"], true);
}

#[test]
fn downloads_file_outside_project_is_outside() {
    let _guard = home_lock().lock().unwrap();
    let home = tempdir().unwrap();
    std::env::set_var("HOME", home.path());
    let project = home.path().join("code").join("app");
    let downloads_project = home.path().join("Downloads").join("didenstuff");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&downloads_project).unwrap();
    let target = downloads_project.join("pose_editor.py");
    fs::write(&target, "").unwrap();
    let cwd = project.canonicalize().unwrap();
    let project_root = cwd.clone();
    let target_abs = target.canonicalize().unwrap();

    let msg = risk_user_message(&format!("edit {}", target.display()), &cwd, &project_root);
    let payload: serde_json::Value = serde_json::from_str(&msg).unwrap();
    assert_eq!(payload["path_scope"], "outside_project");
    assert_eq!(payload["path_in_downloads"], true);
    assert_eq!(
        payload["primary_path"],
        target_abs.to_string_lossy().to_string()
    );
}
