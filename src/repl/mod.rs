//! REPL entrypoints. Interactive terminals use one persistent fullscreen
//! Ratatui app; pipes and prompt files use line-oriented plain output.

mod commands;
mod core;
mod failure;
mod one_shot;
mod report;
mod resume;
mod tui;

pub use commands::CommandResult;

use std::fmt::Write as _;
use std::io::{self, IsTerminal};
use std::time::Instant;

use chrono::Utc;
use tokio::runtime::Runtime as TokioRuntime;

use crate::approval::Level;
use crate::config::{Runtime, nested};
use crate::pricing::refresh;
use crate::summary::{ErrorKind, RunError};
use crate::term::plain::PlainUi;

use core::ReplCore;
pub(crate) use core::{Shared, TurnParams, run_turn_loop};
use one_shot::run_one_shot_async;
use resume::restore_prompt_resume;

pub const DIM: &str = "\x1b[2m";
pub const CYAN: &str = "\x1b[36m";
pub const GREEN: &str = "\x1b[32m";
pub const YELLOW: &str = "\x1b[33m";
pub const RED: &str = "\x1b[31m";
pub const MAGENTA: &str = "\x1b[35m";
pub const BOLD: &str = "\x1b[1m";
pub const RESET: &str = "\x1b[0m";

/// What an interactive turn says when there is nothing to send the prompt to.
///
/// One string, because the REPL and `/recover` are the same situation to whoever
/// reads it. The one-shot path says something else on purpose: a piped run has no
/// `/source` to reach for.
pub(crate) const NO_ACTIVE_SOURCE: &str = "no active source - use /source to select one";

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
    if let Some(text) = instructions_text(rt) {
        parts.push(format!("{DIM}{text}{RESET}"));
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
    if let Some(text) = instructions_text(rt) {
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

/// An `instructions:` segment counting the project files the run loaded, and only
/// when it loaded any.
///
/// The interactive half of reporting what was loaded, which the run summary does
/// for a job nobody is watching. A count rather than the paths, because there can
/// be several and this is one line; `/instructions` lists them with their sizes.
fn instructions_text(rt: &Runtime) -> Option<String> {
    let loaded = nested::sent(rt.prompt()).len();
    (loaded > 0).then(|| format!("instructions:{loaded}"))
}

/// What `/instructions` prints: every project instruction file this run loaded and
/// the bytes it put in front of the model.
///
/// The first thing to reach for when the model ignores a rule the repository
/// states, which is otherwise unanswerable from inside a session: a file that was
/// never found, a subtree file above the directory afi started in, and a rule the
/// model simply did not follow all look identical from the outside.
///
/// The sizes are what was sent rather than what the files hold now, so an edit made
/// mid-session shows up as the difference between this listing and the disk. afi
/// reads them once, at startup, and does not watch them.
///
/// Beside [`instructions_text`] because the two report the same thing at different
/// lengths, and a status line that disagreed with the listing would be worse than
/// either alone.
#[must_use]
pub(crate) fn instructions_listing(rt: &Runtime) -> String {
    let loaded = nested::sent(rt.prompt());
    if loaded.is_empty() {
        return "No project instructions loaded. Start afi with --instructions project \
                to read AGENTS.md and CLAUDE.md from this checkout, or --instructions \
                <path,...> to name files."
            .to_string();
    }
    let total: usize = loaded.iter().map(|(_, bytes, _)| bytes).sum();
    let mut out = format!("{} file(s), {total} bytes sent:", loaded.len());
    for (path, bytes, arrival) in loaded {
        // Infallible: a String write cannot fail.
        let _ = write!(out, "\n  {path} ({bytes} bytes{})", arrival.note());
    }
    out
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
    start_price_refresh(&runtime, &owned);
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
    start_price_refresh(&runtime, rt);
    let mut ui = plain_ui_for(rt);
    runtime.block_on(run_one_shot_async(prompt_file, rt, &mut ui))
}

/// Refresh the cached rate table in the background, for the next run to read.
///
/// Detached rather than awaited: a run must never wait on the rate catalogue,
/// and the
/// table this run bills against was already resolved when the `Runtime` was
/// built. A session that ends before the fetch does simply leaves the cache as
/// it was, and next time the same question gets asked again.
///
/// It runs even for a run that sets no budget and reports no cost, which is the
/// point - the table has to already be current the first time somebody caps a
/// run, not a day after.
fn start_price_refresh(runtime: &TokioRuntime, rt: &Runtime) {
    let fetched = rt
        .pricing
        .as_ref()
        .map_or(String::new(), |p| p.fetched().to_string());
    let today = Utc::now().date_naive().to_string();
    if let Some(plan) = refresh::plan(&rt.env, &fetched, today) {
        runtime.spawn(refresh::run(plan));
    }
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
                // The input never arrived, so nothing was asked and nothing was
                // answered. This used to end the loop quietly and report ok:true,
                // which told CI that a run reading a truncated or non-UTF-8 stream
                // had passed.
                let message = format!("input error: {error}");
                ui.message(MessageKind::Error, message.clone());
                core.record_error(RunError::new(message, ErrorKind::Input));
                core.shutdown(&mut ui);
                break;
            }
        }
    }
    let reported = core.report(started.elapsed(), &mut ui);
    let ok = reported && !core.failed();
    (core.into_runtime(), ok)
}
