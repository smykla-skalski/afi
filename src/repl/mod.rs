//! REPL entrypoints. Interactive terminals use one persistent fullscreen
//! Ratatui app; pipes and prompt files use line-oriented plain output.

mod commands;
mod core;
mod report;
mod tui;

pub use commands::CommandResult;

use std::io::{self, IsTerminal};
use std::time::Instant;

use tokio::runtime::Runtime as TokioRuntime;

use crate::approval::Level;
use crate::config::Runtime;
use crate::term::plain::PlainUi;

use core::{ReplCore, restore_prompt_resume, run_one_shot_async};
pub(crate) use core::{TurnParams, run_turn_loop};

pub const DIM: &str = "\x1b[2m";
pub const CYAN: &str = "\x1b[36m";
pub const GREEN: &str = "\x1b[32m";
pub const YELLOW: &str = "\x1b[33m";
pub const RED: &str = "\x1b[31m";
pub const MAGENTA: &str = "\x1b[35m";
pub const BOLD: &str = "\x1b[1m";
pub const RESET: &str = "\x1b[0m";

/// Styled one-line status retained for plain terminals and CLI consumers.
#[must_use]
pub fn banner(rt: &Runtime) -> String {
    let sep = format!("{DIM} · {RESET}");
    let mut parts = vec![
        format!("{BOLD}afi{RESET}"),
        format!("{CYAN}{}{RESET}", model_name(rt)),
    ];
    if rt.sources.len() > 1
        && let Some(active) = &rt.active
    {
        parts.push(format!("{MAGENTA}{active}{RESET}"));
    }
    parts.push(styled_approval(rt));
    if let Some(text) = tool_policy_text(rt) {
        parts.push(format!("{YELLOW}{text}{RESET}"));
    }
    if let Some(src) = rt.active_source() {
        parts.push(format!("{DIM}{}{RESET}", src.base_url));
    }
    parts.join(&sep)
}

/// Unescaped status rendered by Ratatui header.
#[must_use]
pub(crate) fn header(rt: &Runtime) -> String {
    let mut parts = vec![model_name(rt)];
    if rt.sources.len() > 1
        && let Some(active) = &rt.active
    {
        parts.push(active.clone());
    }
    parts.push(approval_text(rt));
    if let Some(text) = tool_policy_text(rt) {
        parts.push(text);
    }
    if let Some(src) = rt.active_source() {
        parts.push(src.base_url.clone());
    }
    parts.join(" · ")
}

/// A `tools:` segment naming what the run may call, and only when a policy
/// narrows it. An unrestricted run's status line is unchanged, so the segment
/// appearing is itself the signal that something is restricted.
fn tool_policy_text(rt: &Runtime) -> Option<String> {
    let policy = &rt.tool_policy;
    (!policy.is_unrestricted()).then(|| format!("tools:{}", policy.describe()))
}

fn model_name(rt: &Runtime) -> String {
    rt.model.clone().unwrap_or_else(|| "auto".to_string())
}

fn approval_text(rt: &Runtime) -> String {
    if rt.approval.yolo {
        return "yolo".to_string();
    }
    match rt.approval.approve_level {
        None => "prompt:all".to_string(),
        Some(Level::High) => "auto:high".to_string(),
        Some(Level::Medium) => "auto:medium".to_string(),
        Some(Level::Low) => "auto:low".to_string(),
    }
}

fn styled_approval(rt: &Runtime) -> String {
    let text = approval_text(rt);
    let color = match rt.approval.approve_level {
        _ if rt.approval.yolo => GREEN,
        None | Some(Level::Medium) => YELLOW,
        Some(Level::High) => GREEN,
        Some(Level::Low) => DIM,
    };
    format!("{color}{text}{RESET}")
}

/// Select fullscreen or plain frontend once, then reuse one Tokio runtime for
/// whole session. Prompt-file mode never initializes a terminal UI.
///
/// # Panics
///
/// Panics when the process cannot initialize its Tokio runtime.
pub fn run_repl(rt: &mut Runtime) -> bool {
    let mut owned = rt.clone();
    let runtime = TokioRuntime::new().expect("failed to create tokio runtime");
    if let Some(prompt_file) = owned.prompt_file.clone() {
        restore_prompt_resume(&mut owned);
        let mut ui = plain_ui_for(&owned);
        let ok = runtime.block_on(run_one_shot_async(&prompt_file, &owned, &mut ui));
        *rt = owned;
        return ok;
    }
    if io::stdin().is_terminal() && io::stdout().is_terminal() {
        match runtime.block_on(tui::run(owned)) {
            Ok(updated) => *rt = updated,
            Err(error) => eprintln!("afi TUI error: {error}"),
        }
        // A human is watching a TTY session; nothing to report to a caller.
        return true;
    }
    // Piped stdin with no --prompt-file is still a non-interactive run, so it
    // reports and exits like one.
    let (updated, ok) = runtime.block_on(run_plain(owned));
    *rt = updated;
    ok
}

/// Public one-shot helper. Output stays plain even when caller owns a TTY.
///
/// # Panics
///
/// Panics when the process cannot initialize its Tokio runtime.
#[must_use]
pub fn run_one_shot(prompt_file: &str, rt: &Runtime) -> bool {
    let runtime = TokioRuntime::new().expect("failed to create tokio runtime");
    let mut ui = plain_ui_for(rt);
    runtime.block_on(run_one_shot_async(prompt_file, rt, &mut ui))
}

/// A plain ui, with human output moved off stdout when the run summary claims it.
///
/// Otherwise the rendered answer and the JSON share stdout and the summary cannot
/// be parsed - `afi --summary json -f p.txt | jq` fails on the prose.
fn plain_ui_for(rt: &Runtime) -> PlainUi {
    if rt.summary.is_json() {
        PlainUi::diverted()
    } else {
        PlainUi::new()
    }
}

/// Returns the updated runtime and whether every turn succeeded.
async fn run_plain(rt: Runtime) -> (Runtime, bool) {
    let started = Instant::now();
    let mut ui = plain_ui_for(&rt);
    let mut core = ReplCore::new(rt, &mut ui);
    loop {
        match ui.read_prompt("  > ") {
            Ok(input) => {
                if core.handle_input(&input, &mut ui).await.should_quit() {
                    break;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
                core.shutdown(&mut ui);
                break;
            }
            Err(error) => {
                use crate::term::{MessageKind, UserInterface};
                ui.message(MessageKind::Error, format!("input error: {error}"));
                core.shutdown(&mut ui);
                break;
            }
        }
    }
    let reported = core.report(started.elapsed(), &mut ui);
    let ok = reported && !core.failed();
    (core.into_runtime(), ok)
}
