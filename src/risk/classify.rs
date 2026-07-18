//! Action-path extraction, path-scope classification, the classifier user
//! message, and the `confirm` approval gate.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::json;

use super::{ApprovalChoice, EscToChat, RiskClassifier, is_under_path};
use crate::approval::{ApprovalState, Level};

/// Matches `write <path> (N bytes)` action strings.
static WRITE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^write\s+(.+)\s+\(\d+\s+bytes\)$").unwrap());

/// Pull the primary file path from afi's short approval action string.
/// Handles "edit <path>" and "write <path> (N bytes)".
#[must_use]
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
    if let Some(m) = WRITE_RE.captures(action) {
        let p = m.get(1).map_or("", |g| g.as_str().trim());
        return if p.is_empty() {
            None
        } else {
            Some(p.to_string())
        };
    }
    None
}

/// Classify a path as `in_project/outside_project`, `in_cwd`, `in_downloads`, etc.
#[must_use]
pub fn classify_action_path(path: &str, cwd: &Path, project_root: &Path) -> serde_json::Value {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    classify_action_path_with_home(path, cwd, project_root, &home)
}

fn classify_action_path_with_home(
    path: &str,
    cwd: &Path,
    project_root: &Path,
    home: &Path,
) -> serde_json::Value {
    let expanded = expand_tilde(path);
    let abs_path = if expanded.is_absolute() {
        expanded.canonicalize().unwrap_or(expanded)
    } else {
        cwd.join(&expanded)
            .canonicalize()
            .unwrap_or_else(|_| cwd.join(&expanded))
    };
    let home = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
    let downloads = home.join("Downloads");

    let in_project = is_under_path(&abs_path, project_root);
    let in_cwd = is_under_path(&abs_path, cwd);
    let in_downloads = is_under_path(&abs_path, &downloads);

    let rel_home = abs_path.strip_prefix(&home).ok();
    let touches_home_dotdir = rel_home
        .and_then(|r| r.components().next())
        .and_then(|c| c.as_os_str().to_str())
        .is_some_and(|s| s.starts_with('.'));

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
#[must_use]
pub fn risk_user_message(action: &str, cwd: &Path, project_root: &Path) -> String {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    risk_user_message_with_home(action, cwd, project_root, &home)
}

fn risk_user_message_with_home(
    action: &str,
    cwd: &Path,
    project_root: &Path,
    home: &Path,
) -> String {
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
        let classification = classify_action_path_with_home(&path, cwd, project_root, home);
        if let (Some(msg_obj), Some(cls_obj)) = (msg.as_object_mut(), classification.as_object()) {
            for (k, v) in cls_obj {
                msg_obj.insert(k.clone(), v.clone());
            }
        }
    }
    // Sort keys for deterministic output (matches Python json.dumps(sort_keys=True)).
    serde_json::to_string(&msg).unwrap_or_else(|_| msg.to_string())
}

/// Shorten `reason` to 80 chars with an ellipsis so prompts stay one line.
fn short_reason(reason: String) -> String {
    if reason.len() <= 80 {
        reason
    } else {
        format!("{}...", &reason[..77])
    }
}

/// Decide whether to run `action`. Returns `Ok(true)` to proceed, `Ok(false)`
/// to deny, or `Err(EscToChat)` if the user pressed Esc.
///
/// 1. YOLO -> proceed (no call, no prompt).
/// 2. Ask the classifier for a risk level.
/// 3. If `approve_level` is set and level <= `approve_level` -> auto-allow.
/// 4. Otherwise prompt (Y/n/esc) with the level + reason.
///
/// # Errors
/// Returns [`EscToChat`] when the user presses Esc at the approval prompt.
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

    if let Some(threshold) = approval.approve_level
        && level <= threshold
    {
        eprintln!(
            "  \u{21b3} auto-allow [{level}] {action}  ({})",
            short_reason(reason)
        );
        return Ok(true);
    }

    let lvl_color = match level {
        Level::Low => "\x1b[2m",     // DIM
        Level::Medium => "\x1b[33m", // YELLOW
        Level::High => "\x1b[31m",   // RED
    };
    let short = short_reason(reason);
    let prompt = format!(
        "\x1b[33m  allow {action}? {lvl_color}[risk: {level} \u{2014} {short}]\x1b[0m \x1b[33m[Y/n/esc] \x1b[0m"
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
    use tempfile::tempdir;

    /// A mock classifier that returns a fixed level + reason.
    struct MockClassifier {
        level: Level,
        reason: String,
    }
    impl RiskClassifier for MockClassifier {
        fn classify(&self, _action: &str, _cwd: &Path, _project_root: &Path) -> (Level, String) {
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
        let home = tempdir().unwrap();
        let project = home.path().join("Downloads").join("didenstuff");
        fs::create_dir_all(&project).unwrap();
        let target = project.join("pose_editor.py");
        fs::write(&target, "").unwrap();
        let cwd = project.canonicalize().unwrap();
        let project_root = cwd.clone();
        let target_abs = target.canonicalize().unwrap();

        let msg = risk_user_message_with_home(
            &format!("edit {}", target.display()),
            &cwd,
            &project_root,
            home.path(),
        );
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
        let home = tempdir().unwrap();
        let project = home.path().join("code").join("app");
        let downloads_project = home.path().join("Downloads").join("didenstuff");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&downloads_project).unwrap();
        let target = downloads_project.join("pose_editor.py");
        fs::write(&target, "").unwrap();
        let cwd = project.canonicalize().unwrap();
        let project_root = cwd.clone();

        let msg = risk_user_message_with_home(
            &format!("edit {}", target.display()),
            &cwd,
            &project_root,
            home.path(),
        );
        let payload: serde_json::Value = serde_json::from_str(&msg).unwrap();
        assert_eq!(payload["path_scope"], "outside_project");
        assert_eq!(payload["path_in_downloads"], true);
    }
}
