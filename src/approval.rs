//! Approval gating: risk levels (low < medium < high), a strict "prompt all"
//! state, and YOLO mode.
//!
//! `approve_level` is the maximum risk level to AUTO-APPROVE: actions
//! classified at <= `approve_level` run without prompting; anything strictly
//! above prompts. `approve_level == None` means prompt for every classified
//! action (the "prompt all" state). `yolo == true` short-circuits entirely
//! and skips the classifier call.

use std::fmt;

/// Risk level for an action. Order matters: `Low < Medium < High`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    Low,
    Medium,
    High,
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Level::Low => write!(f, "low"),
            Level::Medium => write!(f, "medium"),
            Level::High => write!(f, "high"),
        }
    }
}

/// What kind of approval mode a flag or env value resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalKind {
    /// Auto-approve up to and including this level; prompt above.
    Level(Level),
    /// Prompt for every classified action.
    PromptAll,
    /// Never prompt; skip the classifier entirely.
    Yolo,
}

/// The mutable approval state of a running session.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ApprovalState {
    pub yolo: bool,
    pub approve_level: Option<Level>,
    pub default_approve_level: Option<Level>,
}

/// Accept full words and common abbreviations (med, hi, lo, m, h, l ...).
///
/// Returns the canonical level name or `None` if the input isn't recognised.
#[must_use]
pub fn normalize_level(arg: &str) -> Option<Level> {
    match arg.trim().to_lowercase().as_str() {
        "l" | "lo" | "low" => Some(Level::Low),
        "m" | "med" | "mid" | "medium" => Some(Level::Medium),
        "h" | "hi" | "high" => Some(Level::High),
        _ => None,
    }
}

/// Resolve approval setting aliases.
///
/// Returns `Some(Level(l))`, `Some(PromptAll)`, `Some(Yolo)`, or `None` when
/// unrecognized. Mirrors the Python `_normalize_approval` tuple return.
pub fn normalize_approval(arg: &str) -> Option<ApprovalKind> {
    match arg.trim().to_lowercase().as_str() {
        "all" | "prompt" | "prompt-all" | "strict" | "none" => Some(ApprovalKind::PromptAll),
        "yolo" => Some(ApprovalKind::Yolo),
        other => normalize_level(other).map(ApprovalKind::Level),
    }
}

/// Apply a resolved approval mode to `state`.
///
/// When `update_default` is true, `default_approve_level` tracks
/// `approve_level` so the value persists as the session's default (the env-var
/// and `--approval` flag paths both set it; the bare `--yolo` flag does not).
pub fn apply_approval(state: &mut ApprovalState, kind: ApprovalKind, update_default: bool) -> bool {
    match kind {
        ApprovalKind::Yolo => {
            state.yolo = true;
            state.approve_level = None;
            if update_default {
                state.default_approve_level = None;
            }
        }
        ApprovalKind::PromptAll => {
            state.yolo = false;
            state.approve_level = None;
            if update_default {
                state.default_approve_level = None;
            }
        }
        ApprovalKind::Level(l) => {
            state.yolo = false;
            state.approve_level = Some(l);
            if update_default {
                state.default_approve_level = Some(l);
            }
        }
    }
    true
}

/// Short human-readable label for the approval mode, used by the banner.
#[must_use]
pub fn approval_display(state: &ApprovalState) -> &'static str {
    if state.yolo {
        "off (yolo)"
    } else {
        match state.approve_level {
            None => "all",
            Some(Level::High) => "high",
            Some(Level::Medium) => "medium",
            Some(Level::Low) => "low",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_ordering() {
        assert!(Level::Low < Level::Medium);
        assert!(Level::Medium < Level::High);
    }

    #[test]
    fn normalize_level_aliases() {
        assert_eq!(normalize_level("med"), Some(Level::Medium));
        assert_eq!(normalize_level("HI"), Some(Level::High));
        assert_eq!(normalize_level("nope"), None);
    }

    #[test]
    fn normalize_approval_variants() {
        assert_eq!(normalize_approval("strict"), Some(ApprovalKind::PromptAll));
        assert_eq!(normalize_approval("yolo"), Some(ApprovalKind::Yolo));
        assert_eq!(
            normalize_approval("medium"),
            Some(ApprovalKind::Level(Level::Medium))
        );
        assert_eq!(normalize_approval("garbage"), None);
    }

    #[test]
    fn apply_updates_default_when_requested() {
        let mut s = ApprovalState::default();
        apply_approval(&mut s, ApprovalKind::Level(Level::Medium), true);
        assert_eq!(s.approve_level, Some(Level::Medium));
        assert_eq!(s.default_approve_level, Some(Level::Medium));
        assert!(!s.yolo);
    }

    #[test]
    fn yolo_clears_level_not_default() {
        let mut s = ApprovalState {
            yolo: false,
            approve_level: Some(Level::Medium),
            default_approve_level: Some(Level::Medium),
        };
        // bare --yolo path: yolo=true, approve_level=None, default untouched
        s.yolo = true;
        s.approve_level = None;
        assert!(s.yolo);
        assert_eq!(s.approve_level, None);
        assert_eq!(s.default_approve_level, Some(Level::Medium));
    }
}
