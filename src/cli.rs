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

use serde_json::Value;

use chrono::TimeZone;

use crate::repl::{CYAN, DIM, GREEN, MAGENTA, RESET, YELLOW};
use crate::sessions::{
    list_sessions, load_session, new_session_id, resolve_session, safe_title,
    session_summary_from_file, sessions_dir, write_session, SessionSummary,
    SESSION_LIST_DEFAULT_LIMIT, SESSION_LIST_MAX_LIMIT,
};

/// Resolve a starting session id from CLI flags: `--resume`/`-r` and
/// `--session`.
///
/// `--resume` with no following target resumes the most recent session.
/// With a target it resolves by index/id/prefix/title. `--session <id>`
/// forces an exact id. Returns `None` if no relevant flag is present or
/// `--resume` finds nothing.
pub fn session_id_from_args(
    args: &[String],
    env: &std::collections::HashMap<String, String>,
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
pub fn session_list_page_options(args: &[String]) -> PageOptions {
    let mut page = 1usize;
    let mut limit = SESSION_LIST_DEFAULT_LIMIT;
    let mut query_parts: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    let positive_int = |v: &str, fallback: usize, name: &str, warns: &mut Vec<String>| -> usize {
        match v.parse::<usize>() {
            Ok(n) if n >= 1 => n,
            _ => {
                warns.push(format!("ignored invalid {}: {:?}", name, v));
                fallback
            }
        }
    };

    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--page" || a == "-p" {
            if i + 1 >= args.len() {
                warnings.push(format!("ignored missing value for {}", a));
                i += 1;
                continue;
            }
            page = positive_int(&args[i + 1], page, "page", &mut warnings);
            i += 2;
            continue;
        }
        if let Some(rest) = a.strip_prefix("--page=") {
            page = positive_int(rest, page, "page", &mut warnings);
            i += 1;
            continue;
        }
        if a == "--limit" || a == "-n" {
            if i + 1 >= args.len() {
                warnings.push(format!("ignored missing value for {}", a));
                i += 1;
                continue;
            }
            limit = positive_int(&args[i + 1], limit, "limit", &mut warnings);
            i += 2;
            continue;
        }
        if let Some(rest) = a.strip_prefix("--limit=") {
            limit = positive_int(rest, limit, "limit", &mut warnings);
            i += 1;
            continue;
        }
        query_parts.push(a.clone());
        i += 1;
    }

    limit = limit.clamp(1, SESSION_LIST_MAX_LIMIT);
    let query = query_parts.join(" ");
    let query = if query.trim().is_empty() {
        None
    } else {
        Some(query)
    };
    PageOptions {
        query,
        page,
        limit,
        warnings,
    }
}

/// Compact relative timestamp for the session list.
pub fn fmt_when(ts: f64) -> String {
    if ts <= 0.0 {
        return "?".to_string();
    }
    let now = chrono::Local::now().timestamp() as f64;
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
        chrono::Local
            .timestamp_opt(ts as i64, 0)
            .single()
            .map(|t| t.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| "?".to_string())
    }
}
/// Collapse a path to `~/...` form when it's under the user's home dir.
pub fn display_path(path: Option<&str>) -> Option<String> {
    let path = path?;
    let home = dirs::home_dir()?;
    let expanded = expand_tilde(path, &home);
    let p = std::path::PathBuf::from(&expanded);
    if p == home {
        return Some("~".to_string());
    }
    if let Ok(rest) = p.strip_prefix(&home) {
        return Some(format!("~/{}", rest.to_string_lossy()));
    }
    Some(expanded)
}

/// Expand a leading `~` to the home dir; leave everything else alone.
fn expand_tilde(path: &str, home: &std::path::Path) -> String {
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
        let i = start_index + offset;
        let title = if s.title.is_empty() {
            "(empty)"
        } else {
            &s.title
        };
        let prefix = match current_id {
            Some(cid) if cid == s.id => format!("  {GREEN}\u{25cf}{RESET} "),
            _ => format!("  {DIM}{:>3}{RESET}  ", i),
        };
        let _ = writeln!(out, "{prefix}{MAGENTA}{}{RESET}  {}", s.id, title);

        let mut meta: Vec<String> = vec![format!("{} msg", s.n), fmt_when(s.updated_at)];
        if let Some(src) = &s.source {
            meta.push(format!("source {}", src));
        }
        if let Some(model) = &s.model {
            meta.push(format!("model {}", model));
        }
        if let Some(cwd) = display_path(s.cwd.as_deref()) {
            meta.push(format!("cwd {}", cwd));
        }
        let _ = writeln!(out, "       {DIM}{}{RESET}", meta.join(" \u{00b7} "));

        let detail = &s.description;
        let (label, body): (&str, Option<&String>) = if let Some(d) = detail {
            ("desc", Some(d))
        } else if !s.preview.is_empty() {
            ("first", Some(&s.preview))
        } else {
            ("", None)
        };
        if let Some(b) = body {
            let _ = writeln!(out, "       {DIM}{label}: {b}{RESET}");
        }
    }
}

/// `afi sessions --page N+1 ...` hint string for the "next page" line.
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
    format!("'{}'", escaped)
}

/// Render a session's message history as a one-line-per-message recap. Used
/// on resume and by `/sessions <id>` for its detail view.
pub fn print_transcript<W: Write>(out: &mut W, messages: &[Value], max_chars: usize) -> usize {
    let mut printed = 0;
    for m in messages {
        let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("?");
        if role == "system" {
            continue;
        }
        let mut content: Option<String> = None;
        if m.get("content").is_none() && m.get("tool_calls").is_some() {
            if let Some(arr) = m.get("tool_calls").and_then(|t| t.as_array()) {
                let names: Vec<String> = arr
                    .iter()
                    .filter_map(|tc| {
                        tc.get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|n| n.as_str())
                            .map(|n| format!("{}(...)", n))
                    })
                    .collect();
                content = Some(format!("\u{2192} {}", names.join(", ")));
            }
        } else if let Some(Value::Array(parts)) = m.get("content") {
            let joined: String = parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()).map(String::from))
                .collect::<Vec<_>>()
                .join(" ");
            content = Some(joined);
        } else if let Some(s) = m.get("content").and_then(|c| c.as_str()) {
            content = Some(s.to_string());
        }
        let content = content.unwrap_or_default();
        let content = content.trim().replace('\n', " ");
        let content = if content.chars().count() > max_chars {
            let head: String = content.chars().take(max_chars - 1).collect();
            format!("{}\u{2026}", head)
        } else {
            content
        };
        let color = match role {
            "user" => CYAN,
            "assistant" => GREEN,
            "tool" => DIM,
            _ => "",
        };
        let _ = writeln!(out, "  {}{:>9}{}  {}", color, role, RESET, content);
        printed += 1;
    }
    printed
}

/// Handle `afi sessions [query] [--page N] [--limit N]` (or the
/// `--sessions` / `ls` / `list` aliases). Returns `true` if this was a
/// sessions invocation (and `out` has been written); `false` if `args`
/// isn't a sessions request and the caller should proceed normally.
pub fn cli_sessions<W: Write>(
    args: &[String],
    env: &std::collections::HashMap<String, String>,
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
        let _ = writeln!(out, "{YELLOW}  {}{RESET}", warning);
    }
    if sessions.is_empty() {
        if let Some(q) = &query {
            let _ = writeln!(out, "  no sessions matching {:?} on page {}", q, page);
        } else if page > 1 {
            let _ = writeln!(out, "  no sessions on page {}  ({})", page, dir.display());
        } else {
            let _ = writeln!(out, "  no saved sessions yet  ({})", dir.display());
        }
        return true;
    }
    let where_ = query
        .as_ref()
        .map(|q| format!(" matching {:?}", q))
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

#[allow(dead_code)]
fn _silence_unused() {
    let _ = safe_title(None, 60);
    let _ = new_session_id();
    let _ = session_summary_from_file(Path::new("."), "x");
    let _ = load_session(Path::new("."), "x");
    let _ = write_session(Path::new("."), "x", &mut Vec::new(), None);
}
