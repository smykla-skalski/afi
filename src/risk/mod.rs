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

use std::error::Error;
use std::fmt;

use crate::approval::Level;

mod classify;
pub use classify::{classify_action_path, confirm, extract_action_path, risk_user_message};

/// Control-flow signal: the user pressed Esc at an approval prompt. Not a
/// real error - propagates up from `confirm` through the tool dispatch to
/// the model turn loop, which stops the turn and returns to chat.
#[derive(Debug, Clone)]
pub struct EscToChat(pub String);

impl fmt::Display for EscToChat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "EscToChat({})", self.0)
    }
}

impl Error for EscToChat {}

/// The result of an approval prompt: proceed, deny, or escape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalChoice {
    Yes,
    No,
    Esc,
}

/// Return the git root for `cwd` when available, otherwise `cwd` itself.
#[must_use]
pub fn detect_project_root(cwd: Option<&Path>) -> PathBuf {
    let cwd = cwd.map_or_else(
        || env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        |p| p.canonicalize().unwrap_or_else(|_| p.to_path_buf()),
    );
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
#[must_use]
pub fn is_under_path(path: &Path, root: &Path) -> bool {
    path.starts_with(root)
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
            format!("no classifier available; defaulting to high for: {action}"),
        )
    }
}
