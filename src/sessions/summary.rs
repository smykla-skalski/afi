//! Session summaries (one per file) + listing + target resolution.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::store::session_files_newest;
use super::{safe_title, short_id};
use std::cmp::Ordering;
use std::fs;

/// A scannable one-line summary of a saved session, used by
/// `afi sessions` / `/sessions` / `/resume`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    pub short: String,
    pub title: String,
    pub description: Option<String>,
    pub preview: String,
    pub updated_at: f64,
    pub n: usize,
    pub model: Option<String>,
    pub source: Option<String>,
    pub cwd: Option<String>,
}

/// Build a `SessionSummary` from a filename inside `dir`. Returns `None` on
/// missing/parse error.
pub fn session_summary_from_file(dir: &Path, fname: &str) -> Option<SessionSummary> {
    let path = dir.join(fname);
    let bytes = fs::read(&path).ok()?;
    let data: Value = serde_json::from_slice(&bytes).ok()?;
    data.get("messages")?;

    let sid = data
        .get("id")
        .and_then(|v| v.as_str())
        .map_or_else(|| fname.trim_end_matches(".json").to_string(), String::from);
    let msgs = data.get("messages").and_then(|m| m.as_array());

    let mut preview = String::new();
    if let Some(arr) = msgs {
        for m in arr {
            if m.get("role").and_then(|r| r.as_str()) == Some("user")
                && let Some(c) = m.get("content").and_then(|c| c.as_str())
            {
                preview = safe_title(Some(c), 60).unwrap_or_default();
                break;
            }
        }
    }

    let title = data
        .get("title")
        .and_then(|v| v.as_str())
        .map(String::from)
        .or_else(|| {
            if preview.is_empty() {
                None
            } else {
                Some(preview.clone())
            }
        })
        .unwrap_or_else(|| "(empty)".to_string());

    let n = msgs.map_or(0, |arr| {
        arr.iter()
            .filter(|m| m.get("role").and_then(|r| r.as_str()) != Some("system"))
            .count()
    });

    Some(SessionSummary {
        id: sid.clone(),
        short: short_id(&sid),
        title,
        description: data
            .get("description")
            .and_then(|v| v.as_str())
            .map(String::from),
        preview,
        updated_at: data
            .get("updated_at")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
        n,
        model: data.get("model").and_then(|v| v.as_str()).map(String::from),
        source: data
            .get("source")
            .and_then(|v| v.as_str())
            .map(String::from),
        cwd: data.get("cwd").and_then(|v| v.as_str()).map(String::from),
    })
}

/// True if `query` (lowercased) appears in any of the summary's searchable
/// fields. Empty query matches everything.
#[must_use]
pub fn session_matches_query(summary: &SessionSummary, query: Option<&str>) -> bool {
    let q = match query {
        Some(q) if !q.is_empty() => q.to_lowercase(),
        _ => return true,
    };
    let in_field = |s: Option<&str>| -> bool { s.is_some_and(|v| v.to_lowercase().contains(&q)) };
    in_field(Some(&summary.title))
        || in_field(summary.description.as_deref())
        || in_field(Some(&summary.preview))
        || in_field(Some(&summary.id))
        || in_field(Some(&summary.short))
}

/// Newest-first list of session summaries.
///
/// `limit == None` means no limit. With a `query`, every file is parsed
/// (filtering can match older sessions beyond the first page); without a
/// query, parsing stops as soon as `offset + limit` matches are collected
/// so listing many sessions doesn't require parsing every transcript.
#[must_use]
pub fn list_sessions(
    dir: &Path,
    limit: Option<usize>,
    offset: usize,
    query: Option<&str>,
) -> Vec<SessionSummary> {
    let files = session_files_newest(dir);
    let mut out: Vec<SessionSummary> = Vec::new();
    for fname in files {
        let Some(summary) = session_summary_from_file(dir, &fname) else {
            continue;
        };
        if !session_matches_query(&summary, query) {
            continue;
        }
        out.push(summary);
        if query.is_none()
            && let Some(lim) = limit
            && out.len() >= offset + lim
        {
            break;
        }
    }
    if query.is_some() {
        out.sort_by(|a, b| {
            b.updated_at
                .partial_cmp(&a.updated_at)
                .unwrap_or(Ordering::Equal)
        });
    }
    let end = limit.map(|l| offset + l);
    let start = offset.min(out.len());
    let end = end.map_or(out.len(), |e| e.min(out.len()));
    out[start..end].to_vec()
}

/// Resolve a user-typed target to a session id.
///
/// Accepts: a full id, a numeric index into the recent-sessions list, a
/// unique id prefix, a short id (the 6-hex suffix), or an exact title.
/// Returns `None` if nothing matches or the input is ambiguous.
#[must_use]
pub fn resolve_session(target: &str, sessions: &[SessionSummary]) -> Option<String> {
    let target = target.trim();
    if target.is_empty() {
        return None;
    }
    let ids: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();

    // numeric index → recent-sessions slot (1-based)
    if let Ok(idx) = target.parse::<usize>()
        && idx >= 1
        && idx <= sessions.len()
    {
        return Some(sessions[idx - 1].id.clone());
    }
    // exact id
    if ids.contains(&target) {
        return Some(target.to_string());
    }
    // unique prefix
    let prefixed: Vec<&str> = ids
        .iter()
        .copied()
        .filter(|i| i.starts_with(target))
        .collect();
    if prefixed.len() == 1 {
        return Some(prefixed[0].to_string());
    }
    // short id (6-hex suffix)
    let suffixed: Vec<&str> = ids
        .iter()
        .copied()
        .filter(|i| i.ends_with(&format!("-{target}")) || short_id(i) == target)
        .collect();
    if suffixed.len() == 1 {
        return Some(suffixed[0].to_string());
    }
    // exact title
    let titled: Vec<&str> = sessions
        .iter()
        .filter(|s| s.title == target)
        .map(|s| s.id.as_str())
        .collect();
    if titled.len() == 1 {
        return Some(titled[0].to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use serde_json::json;

    use crate::sessions::new_session_id;
    use crate::sessions::store::write_session;

    fn msg(role: &str, content: &str) -> Value {
        json!({"role": role, "content": content})
    }

    fn write(dir: &Path, sid: &str, msgs: &[Value], meta: Option<&Value>) {
        let mut msgs = msgs.to_vec();
        write_session(dir, sid, &mut msgs, meta).unwrap();
    }

    #[test]
    fn list_newest_first_with_preview() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        for (i, txt) in ["aaa", "bbb", "ccc"].iter().enumerate() {
            let sid = format!("20250101-12000{i}-order{i}");
            write(
                dir,
                &sid,
                &[msg("user", txt)],
                Some(&json!({"updated_at": 100 + i})),
            );
            // Force mtime ordering since updated_at in JSON doesn't set mtime
            // on its own; we rely on write_session aligning mtime to updated_at.
        }
        let sessions = list_sessions(dir, None, 0, None);
        assert!(!sessions.is_empty());
        // newest (last written) should be first
        assert!(sessions[0].id.ends_with("-order2"));
        let previews: HashSet<&str> = sessions.iter().map(|s| s.preview.as_str()).collect();
        assert!(previews.contains("aaa"));
        assert!(previews.contains("bbb"));
        assert!(previews.contains("ccc"));
    }

    #[test]
    fn list_stops_at_limit_without_query() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        for i in 0..20 {
            let sid = format!("20250101-1300{i:02}-lim{i:02}");
            write(
                dir,
                &sid,
                &[msg("user", &format!("limited {i:02}"))],
                Some(&json!({"updated_at": 200 + i})),
            );
        }
        let sessions = list_sessions(dir, Some(5), 0, None);
        assert_eq!(sessions.len(), 5);
    }

    #[test]
    fn resolve_session_supports_index_prefix_title() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let sid = new_session_id();
        write(
            dir,
            &sid,
            &[msg("user", "unique title here")],
            Some(&json!({"title": "unique title here"})),
        );
        let sessions = list_sessions(dir, Some(50), 0, None);
        assert_eq!(
            resolve_session("1", &sessions),
            Some(sessions[0].id.clone())
        );
        assert_eq!(
            resolve_session(&sessions[0].id, &sessions),
            Some(sessions[0].id.clone())
        );
        // unique prefix (use ~18 chars of the id)
        let prefix: String = sessions[0].id.chars().take(18).collect();
        assert_eq!(
            resolve_session(&prefix, &sessions),
            Some(sessions[0].id.clone())
        );
        // exact title
        assert_eq!(resolve_session("unique title here", &sessions), Some(sid));
        // unknown → None
        assert_eq!(resolve_session("nope-no-such", &sessions), None);
    }

    #[test]
    fn matches_query_searches_all_fields() {
        let s = SessionSummary {
            id: "20250101-120000-abc123".to_string(),
            short: "abc123".to_string(),
            title: "Refactor auth".to_string(),
            description: Some("working on login".to_string()),
            preview: "first message".to_string(),
            updated_at: 0.0,
            n: 0,
            model: None,
            source: None,
            cwd: None,
        };
        assert!(session_matches_query(&s, Some("refactor")));
        assert!(session_matches_query(&s, Some("login")));
        assert!(session_matches_query(&s, Some("abc123")));
        assert!(!session_matches_query(&s, Some("zzz")));
        assert!(session_matches_query(&s, None));
    }
}
