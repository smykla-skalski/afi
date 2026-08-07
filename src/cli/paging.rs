//! What the arguments after `afi sessions` mean: which page, how many, and what
//! is left over as the search query.
//!
//! Split from `cli` because it answers a different question - `cli` renders a
//! listing, this decides what was asked for - and because the answer now has
//! three shapes rather than two: a page, a mistake worth warning about, and an
//! argument the listing cannot honour at all.

use crate::sessions::{SESSION_LIST_DEFAULT_LIMIT, SESSION_LIST_MAX_LIMIT};

/// What `afi sessions` did with the arguments it was given.
///
/// A bool could say "handled" but not "handled, and the exit code has to say so":
/// an argument the listing cannot honour must not print a page from the default
/// directory and call that a success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listing {
    /// Not a `sessions` invocation. The caller carries on.
    NotAsked,
    /// Printed, and the caller is done.
    Printed,
    /// Refused, carrying why. The caller reports these and exits non-zero.
    Refused(Vec<String>),
}

/// Parsed paging flags + leftover query from a `sessions` invocation.
#[derive(Debug, Clone)]
pub struct PageOptions {
    pub query: Option<String>,
    pub page: usize,
    pub limit: usize,
    pub warnings: Vec<String>,
    /// Arguments the listing cannot honour, which stop it rather than warn about
    /// it - see [`apply_page_arg`].
    pub refusals: Vec<String>,
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
        refusals: Vec::new(),
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
        refusals: acc.refusals,
    }
}

struct PageAccum {
    page: usize,
    limit: usize,
    query_parts: Vec<String>,
    warnings: Vec<String>,
    refusals: Vec<String>,
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
        // A long flag the listing does not have was searched for as text, so
        // `afi sessions --read-only` looked for sessions titled "--read-only" and
        // found none - and any flag meant for a run, like `--config`, was taken
        // the same way while quietly not applying. Nobody searches for a `--`
        // word, so this is a mistake every time. A single dash stays query text:
        // a title may well start with one.
        _ if a.starts_with("--") => {
            acc.refusals
                .push(format!("{a} is not an argument afi sessions takes"));
            1
        }
        _ => {
            acc.query_parts.push(a.to_string());
            1
        }
    }
}
