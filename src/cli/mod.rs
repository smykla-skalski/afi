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
use crate::sessions::{
    SESSION_LIST_DEFAULT_LIMIT, SESSION_LIST_MAX_LIMIT, SessionSummary, list_sessions,
    resolve_session, sessions_dir,
};
use std::collections::HashMap;
use std::hash::BuildHasher;
use std::path::PathBuf;

mod transcript;
pub use transcript::print_transcript;

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

/// Parsed paging flags + leftover query from a `sessions` invocation.
#[derive(Debug, Clone)]
pub struct PageOptions {
    pub query: Option<String>,
    pub page: usize,
    pub limit: usize,
    pub warnings: Vec<String>,
}

/// Parse `--page`/`-p` / `--limit`/`-n` flags (and `--page=N` / `--limit=N`
/// equals forms) out of `args`. Anything that isn't a paging flag becomes the
/// search query. `limit` is clamped to `[1, SESSION_LIST_MAX_LIMIT]`.
#[must_use]
pub fn session_list_page_options(args: &[String]) -> PageOptions {
    let mut acc = PageAccum {
        page: 1,
        limit: SESSION_LIST_DEFAULT_LIMIT,
        query_parts: Vec::new(),
        warnings: Vec::new(),
    };
    let mut i = 0;
    while i < args.len() {
        i += apply_page_arg(&mut acc, &args[i], args.get(i + 1).map(String::as_str));
    }

    let limit = acc.limit.clamp(1, SESSION_LIST_MAX_LIMIT);
    let joined = acc.query_parts.join(" ");
    let query = if joined.trim().is_empty() {
        None
    } else {
        Some(joined)
    };
    PageOptions {
        query,
        page: acc.page,
        limit,
        warnings: acc.warnings,
    }
}

struct PageAccum {
    page: usize,
    limit: usize,
    query_parts: Vec<String>,
    warnings: Vec<String>,
}

/// Parse `v` as a positive integer, or warn and keep `fallback`.
fn positive_int(v: &str, fallback: usize, name: &str, warns: &mut Vec<String>) -> usize {
    match v.parse::<usize>() {
        Ok(n) if n >= 1 => n,
        _ => {
            warns.push(format!("ignored invalid {name}: {v:?}"));
            fallback
        }
    }
}

/// Consume a `--flag value` pair into `slot`, warning if the value is missing.
/// Returns the number of args consumed (2 with a value, else 1).
fn take_num(
    slot: &mut usize,
    name: &str,
    flag: &str,
    next: Option<&str>,
    warns: &mut Vec<String>,
) -> usize {
    if let Some(v) = next {
        *slot = positive_int(v, *slot, name, warns);
        2
    } else {
        warns.push(format!("ignored missing value for {flag}"));
        1
    }
}

/// Apply one `sessions` list arg to `acc`. Returns args consumed.
fn apply_page_arg(acc: &mut PageAccum, a: &str, next: Option<&str>) -> usize {
    if let Some(rest) = a.strip_prefix("--page=") {
        acc.page = positive_int(rest, acc.page, "page", &mut acc.warnings);
        return 1;
    }
    if let Some(rest) = a.strip_prefix("--limit=") {
        acc.limit = positive_int(rest, acc.limit, "limit", &mut acc.warnings);
        return 1;
    }
    match a {
        "--page" | "-p" => take_num(&mut acc.page, "page", a, next, &mut acc.warnings),
        "--limit" | "-n" => take_num(&mut acc.limit, "limit", a, next, &mut acc.warnings),
        _ => {
            acc.query_parts.push(a.to_string());
            1
        }
    }
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
    for (offset, s) in sessions.iter().enumerate() {
        print_session_row(out, start_index + offset, s, current_id);
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
) {
    let title = if s.title.is_empty() {
        "(empty)"
    } else {
        &s.title
    };
    let prefix = match current_id {
        Some(cid) if cid == s.id => format!("  {GREEN}\u{25cf}{RESET} "),
        _ => format!("  {DIM}{i:>3}{RESET}  "),
    };
    let _ = writeln!(out, "{prefix}{MAGENTA}{}{RESET}  {}", s.id, title);
    let _ = writeln!(out, "       {DIM}{}{RESET}", session_meta(s));
    if let Some(detail) = session_detail(s) {
        let _ = writeln!(out, "       {DIM}{detail}{RESET}");
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
/// `--sessions` / `ls` / `list` aliases). Returns `true` if this was a
/// sessions invocation (and `out` has been written); `false` if `args`
/// isn't a sessions request and the caller should proceed normally.
pub fn cli_sessions<W: Write, S: BuildHasher>(
    args: &[String],
    env: &HashMap<String, String, S>,
    out: &mut W,
) -> bool {
    if args.is_empty() {
        return false;
    }
    let first = args[0].as_str();
    if !matches!(first, "sessions" | "--sessions" | "ls" | "list") {
        return false;
    }
    let dir = sessions_dir(env);
    let PageOptions {
        query,
        page,
        limit,
        warnings,
    } = session_list_page_options(&args[1..]);
    let offset = (page - 1) * limit;
    let fetched = list_sessions(&dir, Some(limit + 1), offset, query.as_deref());
    let sessions: Vec<SessionSummary> = fetched.into_iter().take(limit).collect();
    let has_next = sessions.len() == limit
        && list_sessions(&dir, Some(1), offset + limit, query.as_deref()).len() == 1;
    for warning in &warnings {
        let _ = writeln!(out, "{YELLOW}  {warning}{RESET}");
    }
    if sessions.is_empty() {
        print_no_sessions(out, query.as_deref(), page, &dir);
        return true;
    }
    let where_ = query
        .as_ref()
        .map(|q| format!(" matching {q:?}"))
        .unwrap_or_default();
    let _ = writeln!(
        out,
        "{DIM}  sessions{} \u{00b7} page {} \u{00b7} {}{RESET}",
        where_,
        page,
        dir.display()
    );
    print_session_list(out, &sessions, offset + 1, None);
    let _ = writeln!(
        out,
        "{DIM}  resume with: afi --resume <n|short-id|title>{RESET}"
    );
    if has_next {
        let _ = writeln!(
            out,
            "{DIM}  next page: {}{RESET}",
            session_next_hint("afi sessions", page, limit, query.as_deref())
        );
    }
    true
}
