//! REPL helpers. Phase 1 only has the banner; the main loop arrives in
//! phase 8.

use crate::approval::Level;
use crate::config::Runtime;

// ANSI codes (a small subset; phase 7 expands this when ratatui lands).
pub const DIM: &str = "\x1b[2m";
pub const CYAN: &str = "\x1b[36m";
pub const GREEN: &str = "\x1b[32m";
pub const YELLOW: &str = "\x1b[33m";
pub const RED: &str = "\x1b[31m";
pub const MAGENTA: &str = "\x1b[35m";
pub const BOLD: &str = "\x1b[1m";
pub const RESET: &str = "\x1b[0m";

/// The startup / post-switch banner: model name, active source (when more
/// than one is configured), approval mode, and endpoint. Printed into the
/// normal scrollback - no pinned status bar, so terminal scrollback works.
pub fn banner(rt: &Runtime) -> String {
    let sep = format!("{DIM} \u{00b7} {RESET}");
    let model = rt.model.clone().unwrap_or_else(|| "auto".to_string());
    let mut parts = vec![
        format!("{BOLD}minion{RESET}"),
        format!("{CYAN}{model}{RESET}"),
    ];
    if rt.sources.len() > 1 {
        if let Some(active) = &rt.active {
            parts.push(format!("{MAGENTA}{active}{RESET}"));
        }
    }
    if rt.approval.yolo {
        parts.push(format!("{GREEN}yolo{RESET}"));
    } else {
        match rt.approval.approve_level {
            None => parts.push(format!("{YELLOW}prompt:all{RESET}")),
            Some(Level::High) => parts.push(format!("{GREEN}auto:high{RESET}")),
            Some(Level::Medium) => parts.push(format!("{YELLOW}auto:medium{RESET}")),
            Some(Level::Low) => parts.push(format!("{DIM}auto:low{RESET}")),
        }
    }
    if let Some(src) = rt.active_source() {
        parts.push(format!("{DIM}{}{RESET}", src.base_url));
    }
    parts.join(&sep)
}
