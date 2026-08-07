//! CLI helpers: the `afi sessions` subcommand, `--resume`/`--session`
//! resolution, and the listing/transcript printers.
//!
//! Two entrypoints:
//! - `session_id_from_args(args, dir)` — resolves the starting session id
//!   from `--resume` / `--session` flags (used at startup)
//! - `cli_sessions(args, dir, out)` — handles `afi sessions [query]` and
//!   returns `true` if it consumed the request (caller should then exit)

use std::io::Write;
use std::path::Path;

use std::time::{Duration, UNIX_EPOCH};

use chrono::{DateTime, Local};

use crate::util::now_secs_f64;

use crate::repl::{DIM, GREEN, MAGENTA, RESET, YELLOW};
use crate::sessions::{SessionSummary, list_sessions, resolve_session, sessions_dir};
use std::collections::HashMap;
use std::hash::BuildHasher;
use std::path::PathBuf;

mod meta;
pub use meta::cli_meta;

mod paging;
pub use paging::{Listing, PageOptions, session_list_page_options};

mod transcript;
pub use transcript::print_transcript;

#[derive(Clone, Copy)]
struct CliStyle(bool);

impl CliStyle {
    fn color(self, color: &'static str) -> &'static str {
        if self.0 { color } else { "" }
    }
}

/// Resolve a starting session id from CLI flags: `--resume`/`-r` and
/// `--session`.
///
/// `--resume` with no following target resumes the most recent session.
/// With a target it resolves by index/id/prefix/title. `--session <id>`
/// forces an exact id. Returns `None` if no relevant flag is present or
/// `--resume` finds nothing.
#[must_use]
pub fn session_id_from_args<S: BuildHasher>(
    args: &[String],
    env: &HashMap<String, String, S>,
) -> Option<String> {
    let dir = sessions_dir(env);
    let mut out: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--resume" || a == "-r" {
            if i + 1 < args.len() && !args[i + 1].starts_with('-') {
                let target = &args[i + 1];
                let sessions = list_sessions(&dir, Some(50), 0, None);
                out = resolve_session(target, &sessions).or_else(|| Some(target.clone()));
                i += 2;
            } else {
                let sessions = list_sessions(&dir, Some(1), 0, None);
                out = sessions.first().map(|s| s.id.clone());
                i += 1;
            }
            continue;
        }
        if a == "--session" && i + 1 < args.len() {
            out = Some(args[i + 1].clone());
            i += 2;
            continue;
        }
        i += 1;
    }
    out
}

/// Compact relative timestamp for the session list.
#[must_use]
pub fn fmt_when(ts: f64) -> String {
    if ts <= 0.0 || !ts.is_finite() {
        return "?".to_string();
    }
    let now = now_secs_f64();
    let delta = now - ts;
    if delta < 60.0 {
        "just now".to_string()
    } else if delta < 3600.0 {
        format!("{:.0}m ago", delta / 60.0)
    } else if delta < 86400.0 {
        format!("{:.0}h ago", delta / 3600.0)
    } else if delta < 86400.0 * 7.0 {
        format!("{:.0}d ago", delta / 86400.0)
    } else {
        let when = UNIX_EPOCH + Duration::from_secs_f64(ts);
        let dt: DateTime<Local> = when.into();
        dt.format("%Y-%m-%d").to_string()
    }
}
/// Collapse a path to `~/...` form when it's under the user's home dir.
#[must_use]
pub fn display_path(path: Option<&str>) -> Option<String> {
    let path = path?;
    let home = dirs::home_dir()?;
    let expanded = expand_tilde(path, &home);
    let p = PathBuf::from(&expanded);
    if p == home {
        return Some("~".to_string());
    }
    if let Ok(rest) = p.strip_prefix(&home) {
        return Some(format!("~/{}", rest.to_string_lossy()));
    }
    Some(expanded)
}

/// Expand a leading `~` to the home dir; leave everything else alone.
fn expand_tilde(path: &str, home: &Path) -> String {
    if path == "~" {
        return home.to_string_lossy().to_string();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return home.join(rest).to_string_lossy().to_string();
    }
    path.to_string()
}

/// Print a session list with the same layout as the Python `_print_session_list`.
pub fn print_session_list<W: Write>(
    out: &mut W,
    sessions: &[SessionSummary],
    start_index: usize,
    current_id: Option<&str>,
) {
    print_session_list_with_style(out, sessions, start_index, current_id, CliStyle(true));
}

fn print_session_list_with_style<W: Write>(
    out: &mut W,
    sessions: &[SessionSummary],
    start_index: usize,
    current_id: Option<&str>,
    style: CliStyle,
) {
    for (offset, s) in sessions.iter().enumerate() {
        print_session_row(out, start_index + offset, s, current_id, style);
    }
}

/// The ` \u{00b7} `-joined metadata line (msg count, when, source/model/cwd).
fn session_meta(s: &SessionSummary) -> String {
    let mut meta: Vec<String> = vec![format!("{} msg", s.n), fmt_when(s.updated_at)];
    if let Some(src) = &s.source {
        meta.push(format!("source {src}"));
    }
    if let Some(model) = &s.model {
        meta.push(format!("model {model}"));
    }
    if let Some(cwd) = display_path(s.cwd.as_deref()) {
        meta.push(format!("cwd {cwd}"));
    }
    meta.join(" \u{00b7} ")
}

/// The optional `desc:`/`first:` detail line for a session.
fn session_detail(s: &SessionSummary) -> Option<String> {
    if let Some(d) = &s.description {
        return Some(format!("desc: {d}"));
    }
    if !s.preview.is_empty() {
        return Some(format!("first: {}", s.preview));
    }
    None
}

fn print_session_row<W: Write>(
    out: &mut W,
    i: usize,
    s: &SessionSummary,
    current_id: Option<&str>,
    style: CliStyle,
) {
    let dim = style.color(DIM);
    let green = style.color(GREEN);
    let magenta = style.color(MAGENTA);
    let reset = style.color(RESET);
    let title = if s.title.is_empty() {
        "(empty)"
    } else {
        &s.title
    };
    let prefix = match current_id {
        Some(cid) if cid == s.id => format!("  {green}\u{25cf}{reset} "),
        _ => format!("  {dim}{i:>3}{reset}  "),
    };
    let _ = writeln!(out, "{prefix}{magenta}{}{reset}  {title}", s.id);
    let _ = writeln!(out, "       {dim}{}{reset}", session_meta(s));
    if let Some(detail) = session_detail(s) {
        let _ = writeln!(out, "       {dim}{detail}{reset}");
    }
}

/// `afi sessions --page N+1 ...` hint string for the "next page" line.
#[must_use]
pub fn session_next_hint(command: &str, page: usize, limit: usize, query: Option<&str>) -> String {
    let q = query
        .map(|q| format!(" {}", shell_quote(q)))
        .unwrap_or_default();
    format!("{}{} --page {} --limit {}", command, q, page + 1, limit)
}

// Minimal shell-quote (single quotes, escapes embedded single quotes). Used
// for the next-page hint; not a general-purpose quoter.
fn shell_quote(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '/' || c == '.')
    {
        return s.to_string();
    }
    let escaped = s.replace('\'', "'\\''");
    format!("'{escaped}'")
}

/// Print the "no sessions" message variant for an empty listing.
fn print_no_sessions<W: Write>(out: &mut W, query: Option<&str>, page: usize, dir: &Path) {
    if let Some(q) = query {
        let _ = writeln!(out, "  no sessions matching {q:?} on page {page}");
    } else if page > 1 {
        let _ = writeln!(out, "  no sessions on page {}  ({})", page, dir.display());
    } else {
        let _ = writeln!(out, "  no saved sessions yet  ({})", dir.display());
    }
}

/// Handle `afi sessions [query] [--page N] [--limit N]` (or the
/// `--sessions` / `ls` / `list` aliases). See [`Listing`] for what the answer
/// means; anything but [`Listing::NotAsked`] means the caller is done with argv.
pub fn cli_sessions<W: Write, S: BuildHasher>(
    args: &[String],
    env: &HashMap<String, String, S>,
    out: &mut W,
) -> Listing {
    cli_sessions_with_style(args, env, out, true)
}

/// TTY-aware variant used by the binary while the original public helper
/// retains its styled rendering contract for existing callers.
pub fn cli_sessions_with_style<W: Write, S: BuildHasher>(
    args: &[String],
    env: &HashMap<String, String, S>,
    out: &mut W,
    styled: bool,
) -> Listing {
    if args.is_empty() {
        return Listing::NotAsked;
    }
    let first = args[0].as_str();
    if !matches!(first, "sessions" | "--sessions" | "ls" | "list") {
        return Listing::NotAsked;
    }
    let dir = sessions_dir(env);
    let PageOptions {
        query,
        page,
        limit,
        warnings,
        refusals,
    } = session_list_page_options(&args[1..]);
    if !refusals.is_empty() {
        return Listing::Refused(refusals);
    }
    let offset = (page - 1) * limit;
    let fetched = list_sessions(&dir, Some(limit + 1), offset, query.as_deref());
    let sessions: Vec<SessionSummary> = fetched.into_iter().take(limit).collect();
    let has_next = sessions.len() == limit
        && list_sessions(&dir, Some(1), offset + limit, query.as_deref()).len() == 1;
    let style = CliStyle(styled);
    let dim = style.color(DIM);
    let reset = style.color(RESET);
    let yellow = style.color(YELLOW);
    for warning in &warnings {
        let _ = writeln!(out, "{yellow}  {warning}{reset}");
    }
    if sessions.is_empty() {
        print_no_sessions(out, query.as_deref(), page, &dir);
        return Listing::Printed;
    }
    let where_ = query
        .as_ref()
        .map(|q| format!(" matching {q:?}"))
        .unwrap_or_default();
    let _ = writeln!(
        out,
        "{dim}  sessions{} \u{00b7} page {} \u{00b7} {}{reset}",
        where_,
        page,
        dir.display()
    );
    print_session_list_with_style(out, &sessions, offset + 1, None, style);
    let _ = writeln!(
        out,
        "{dim}  resume with: afi --resume <n|short-id|title>{reset}"
    );
    if has_next {
        let _ = writeln!(
            out,
            "{dim}  next page: {}{reset}",
            session_next_hint("afi sessions", page, limit, query.as_deref())
        );
    }
    Listing::Printed
}
