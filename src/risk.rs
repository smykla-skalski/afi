//! Risk classifier and approval gating for tool calls.
//!
//! One cheap non-streaming model call per write/bash action classifies it as
//! low/medium/high. The approval mode controls the max level to auto-approve;
//! anything above prompts the user with Y/n/esc. Esc stops the turn and drops
//! back to chat (the `EscToChat` control-flow signal). YOLO mode skips the
//! classifier entirely.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::json;

use crate::approval::{ApprovalState, Level};

/// Control-flow signal: the user pressed Esc at an approval prompt. Not a
/// real error - propagates up from `confirm` through the tool dispatch to
/// the model turn loop, which stops the turn and returns to chat.
#[derive(Debug, Clone)]
pub struct EscToChat(pub String);

impl std::fmt::Display for EscToChat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EscToChat({})", self.0)
    }
}

impl std::error::Error for EscToChat {}

/// The result of an approval prompt: proceed, deny, or escape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalChoice {
    Yes,
    No,
    Esc,
}

/// Return the git root for `cwd` when available, otherwise `cwd` itself.
pub fn detect_project_root(cwd: Option<&Path>) -> PathBuf {
    let cwd = cwd
        .map(|p| p.canonicalize().unwrap_or_else(|_| p.to_path_buf()))
        .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    // Try `git rev-parse --show-toplevel`.
    if let Ok(output) = Command::new("git")
        .args([
            "-C",
            cwd.to_str().unwrap_or("."),
            "rev-parse",
            "--show-toplevel",
        ])
        .output()
    {
        if output.status.success() {
            let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !root.is_empty() {
                let root_path = PathBuf::from(&root);
                return root_path.canonicalize().unwrap_or(root_path);
            }
        }
    }
    cwd
}

/// True if `path` is under `root` (or equal to it).
pub fn is_under_path(path: &Path, root: &Path) -> bool {
    path.starts_with(root)
}

/// Pull the primary file path from Minion's short approval action string.
/// Handles "edit <path>" and "write <path> (N bytes)".
pub fn extract_action_path(action: &str) -> Option<String> {
    let action = action.trim();
    if let Some(rest) = action.strip_prefix("edit ") {
        let p = rest.trim();
        return if p.is_empty() {
            None
        } else {
            Some(p.to_string())
        };
    }
    static WRITE_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"^write\s+(.+)\s+\(\d+\s+bytes\)$").unwrap());
    if let Some(m) = WRITE_RE.captures(action) {
        let p = m.get(1).map(|g| g.as_str().trim()).unwrap_or("");
        return if p.is_empty() {
            None
        } else {
            Some(p.to_string())
        };
    }
    None
}

/// Classify a path as in_project/outside_project, in_cwd, in_downloads, etc.
pub fn classify_action_path(path: &str, cwd: &Path, project_root: &Path) -> serde_json::Value {
    let expanded = expand_tilde(path);
    let abs_path = if expanded.is_absolute() {
        expanded.canonicalize().unwrap_or(expanded)
    } else {
        cwd.join(&expanded)
            .canonicalize()
            .unwrap_or_else(|_| cwd.join(&expanded))
    };
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let home = home.canonicalize().unwrap_or(home);
    let downloads = home.join("Downloads");

    let in_project = is_under_path(&abs_path, project_root);
    let in_cwd = is_under_path(&abs_path, cwd);
    let in_downloads = is_under_path(&abs_path, &downloads);

    let rel_home = abs_path.strip_prefix(&home).ok();
    let touches_home_dotdir = rel_home
        .and_then(|r| r.components().next())
        .and_then(|c| c.as_os_str().to_str())
        .map(|s| s.starts_with('.'))
        .unwrap_or(false);

    json!({
        "primary_path": abs_path.to_string_lossy(),
        "path_scope": if in_project { "in_project" } else { "outside_project" },
        "path_in_cwd": in_cwd,
        "path_in_downloads": in_downloads,
        "path_touches_home_dotdir": touches_home_dotdir,
    })
}

fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(rest);
    }
    PathBuf::from(path)
}

/// Build the JSON user message for the risk classifier.
pub fn risk_user_message(action: &str, cwd: &Path, project_root: &Path) -> String {
    let mut msg = json!({
        "action": action,
        "cwd": cwd.to_string_lossy(),
        "project_root": project_root.to_string_lossy(),
        "primary_path": null,
        "path_scope": "unknown",
        "scope_guidance": "Use project_root/path_scope for outside-project decisions. \
            A file under project_root is in-project even when project_root is inside ~/Downloads.",
    });
    if let Some(path) = extract_action_path(action) {
        let classification = classify_action_path(&path, cwd, project_root);
        if let (Some(msg_obj), Some(cls_obj)) = (msg.as_object_mut(), classification.as_object()) {
            for (k, v) in cls_obj {
                msg_obj.insert(k.clone(), v.clone());
            }
        }
    }
    // Sort keys for deterministic output (matches Python json.dumps(sort_keys=True)).
    serde_json::to_string(&msg).unwrap_or_else(|_| msg.to_string())
}

/// The risk classifier system prompt.
pub const RISK_SYSTEM: &str = "You are a risk classifier for a coding agent's tool calls. \
    Given one tool action as JSON, respond with ONLY a JSON object of the form \
    {\"level\": \"low\"|\"medium\"|\"high\", \"reason\": \"<one short sentence>\"}.\n\
    The user JSON includes cwd, project_root, and when possible path_scope. \
    For outside-project decisions, trust project_root/path_scope over folder-name heuristics. \
    If path_scope is in_project, do not classify high merely because the path is under ~/Downloads; \
    projects can live in Downloads. If path_scope is outside_project, treat that as outside the project.\n\
    Levels:\n\
    - low: read-only or trivially reversible (ls, cat, grep, git status, mkdir, touch, file reads).\n\
    - medium: modifies state but contained/reversible (writing a single file, editing a file, cp, mv, \
    pip install in a venv, running tests, git commit).\n\
    - high: destructive, hard to reverse, or broad scope (rm -rf, git push --force, git reset --hard, \
    dd, chmod -R, writing outside the project, network sends to external hosts, killing processes, \
    system-level changes, anything touching dotfiles in $HOME).\n\
    When in doubt, classify higher. Output ONLY the JSON, no preamble.";

/// The result of a risk assessment: (level, reason).
pub type RiskAssessment = (Level, String);

/// A trait for the risk classifier's model client. The real implementation
/// (phase 5) makes an HTTP call; tests can mock it.
pub trait RiskClassifier {
    fn classify(&self, action: &str, cwd: &Path, project_root: &Path) -> RiskAssessment;
}

/// A no-op classifier that always returns "high" (the safest default).
/// Used when YOLO is off but no client is available (e.g. during startup).
pub struct HighDefaultClassifier;

impl RiskClassifier for HighDefaultClassifier {
    fn classify(&self, action: &str, _cwd: &Path, _project_root: &Path) -> RiskAssessment {
        (
            Level::High,
            format!(
                "no classifier available; defaulting to high for: {}",
                action
            ),
        )
    }
}

/// Decide whether to run `action`. Returns `Ok(true)` to proceed, `Ok(false)`
/// to deny, or `Err(EscToChat)` if the user pressed Esc.
///
/// 1. YOLO -> proceed (no call, no prompt).
/// 2. Ask the classifier for a risk level.
/// 3. If approve_level is set and level <= approve_level -> auto-allow.
/// 4. Otherwise prompt (Y/n/esc) with the level + reason.
pub fn confirm(
    action: &str,
    approval: &ApprovalState,
    classifier: &dyn RiskClassifier,
    cwd: &Path,
    project_root: &Path,
    ask: &dyn Fn(&str) -> ApprovalChoice,
) -> Result<bool, EscToChat> {
    if approval.yolo {
        return Ok(true);
    }
    let (level, reason) = classifier.classify(action, cwd, project_root);

    if let Some(threshold) = approval.approve_level {
        if level <= threshold {
            // Auto-allow.
            let short = if reason.len() <= 80 {
                reason.clone()
            } else {
                format!("{}...", &reason[..77])
            };
            eprintln!("  \u{21b3} auto-allow [{}] {}  ({})", level, action, short);
            return Ok(true);
        }
    }

    let short = if reason.len() <= 80 {
        reason.clone()
    } else {
        format!("{}...", &reason[..77])
    };
    let lvl_color = match level {
        Level::Low => "\x1b[2m",     // DIM
        Level::Medium => "\x1b[33m", // YELLOW
        Level::High => "\x1b[31m",   // RED
    };
    let prompt = format!(
        "\x1b[33m  allow {}? {}[risk: {} \u{2014} {}]\x1b[0m \x1b[33m[Y/n/esc] \x1b[0m",
        action, lvl_color, level, short
    );
    match ask(&prompt) {
        ApprovalChoice::Esc => Err(EscToChat(action.to_string())),
        ApprovalChoice::No => Ok(false),
        ApprovalChoice::Yes => Ok(true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use std::sync::{Mutex, OnceLock};
    use tempfile::tempdir;

    /// Serialize tests that set HOME (env vars aren't thread-safe).
    fn home_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    /// A mock classifier that returns a fixed level + reason.
    struct MockClassifier {
        level: Level,
        reason: String,
    }
    impl RiskClassifier for MockClassifier {
        fn classify(&self, _action: &str, _cwd: &Path, _project_root: &Path) -> RiskAssessment {
            (self.level, self.reason.clone())
        }
    }

    #[test]
    fn extract_action_path_edit() {
        assert_eq!(
            extract_action_path("edit src/main.rs"),
            Some("src/main.rs".to_string())
        );
    }

    #[test]
    fn extract_action_path_write() {
        assert_eq!(
            extract_action_path("write src/main.rs (42 bytes)"),
            Some("src/main.rs".to_string())
        );
    }

    #[test]
    fn extract_action_path_unknown() {
        assert_eq!(extract_action_path("run: echo hello"), None);
    }

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

    #[test]
    fn confirm_prompts_above_threshold() {
        let approval = ApprovalState {
            yolo: false,
            approve_level: Some(Level::Low),
            ..ApprovalState::default()
        };
        let classifier = MockClassifier {
            level: Level::Medium,
            reason: "modifies a file".to_string(),
        };
        let result = confirm(
            "write foo.py",
            &approval,
            &classifier,
            Path::new("."),
            Path::new("."),
            &|_| ApprovalChoice::Yes,
        );
        assert!(result.unwrap());
    }

    #[test]
    fn risk_user_message_downloads_in_project() {
        let _guard = home_lock().lock().unwrap();
        let home = tempdir().unwrap();
        // Set HOME so dirs::home_dir() resolves to the temp dir (matches
        // Python's monkeypatch.setenv("HOME", ...)).
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
    fn risk_user_message_downloads_outside_project() {
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

        let msg = risk_user_message(&format!("edit {}", target.display()), &cwd, &project_root);
        let payload: serde_json::Value = serde_json::from_str(&msg).unwrap();
        assert_eq!(payload["path_scope"], "outside_project");
        assert_eq!(payload["path_in_downloads"], true);
    }
}
