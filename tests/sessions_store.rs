//! Port of the store portion of `tests/test_sessions.py`: write -> load ->
//! list -> resolve -> delete, plus id/title helpers, in a throwaway temp dir.

use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use afi::sessions::{
    delete_session, list_sessions, load_session, new_session_id, resolve_session, safe_title,
    short_id, write_session,
};
use serde_json::json;

fn msg(role: &str, content: &str) -> serde_json::Value {
    json!({"role": role, "content": content})
}

/// Bump a session file's mtime so newest-first ordering is deterministic.
fn bump_mtime(dir: &Path, sid: &str, secs: i64) {
    let path = dir.join(format!("{sid}.json"));
    let f = fs::OpenOptions::new().write(true).open(&path).unwrap();
    let secs = u64::try_from(secs).unwrap_or(0);
    let times = fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::new(secs, 0));
    let _ = f.set_times(times);
}

#[test]
fn test_write_load_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let sid = "20250101-120000-abc123";
    let mut messages = vec![
        json!({"role": "system", "content": "SYS"}),
        msg("user", "hello there"),
        msg("assistant", "hi!"),
    ];
    let meta = json!({"title": "greeting"});
    write_session(dir, sid, &mut messages, Some(&meta)).unwrap();
    let loaded = load_session(dir, sid).expect("load returned None after write");
    assert_eq!(loaded["messages"], json!(messages));
    assert_eq!(loaded["title"], "greeting");
    assert_eq!(loaded["id"], sid);
    assert_eq!(loaded["schema"], "afi-1");
}

#[test]
fn test_write_session_prunes_empty_assistant_messages() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let sid = "20250101-120000-empty0";
    let mut messages = vec![
        msg("user", "hello"),
        msg("assistant", ""),
        msg("assistant", "   "),
        msg("assistant", "real reply"),
    ];
    write_session(dir, sid, &mut messages, None).unwrap();
    let loaded = load_session(dir, sid).unwrap();
    assert_eq!(
        loaded["messages"],
        json!([msg("user", "hello"), msg("assistant", "real reply"),])
    );
}

#[test]
fn test_write_is_atomic_and_merges_meta() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let sid = "20250101-120000-def456";
    let mut m1 = vec![msg("user", "first")];
    write_session(dir, sid, &mut m1, Some(&json!({"source": "local"}))).unwrap();
    let mut m2 = vec![msg("user", "first"), msg("assistant", "reply")];
    write_session(dir, sid, &mut m2, None).unwrap();
    let loaded = load_session(dir, sid).unwrap();
    assert_eq!(loaded["source"], "local");
    assert_eq!(loaded["messages"].as_array().unwrap().len(), 2);
    let created = loaded["created_at"].as_f64().unwrap();
    let updated = loaded["updated_at"].as_f64().unwrap();
    assert!(created <= updated);
}

#[test]
fn test_load_missing_returns_none() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(load_session(tmp.path(), "does-not-exist-999").is_none());
}

#[test]
fn test_list_sessions_orders_newest_first_with_preview() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    // Write three with explicit mtimes so ordering is deterministic.
    let texts = ["aaa", "bbb", "ccc"];
    let sids: Vec<String> = (0..3)
        .map(|i| format!("20250101-12000{i}-order{i}"))
        .collect();
    for (i, txt) in texts.iter().enumerate() {
        let mut m = vec![msg("user", txt)];
        write_session(dir, &sids[i], &mut m, None).unwrap();
        bump_mtime(dir, &sids[i], 100 + i64::try_from(i).unwrap_or(i64::MAX));
    }
    let sessions = list_sessions(dir, None, 0, None);
    assert_eq!(sessions[0].id, sids[2]);
    let previews: HashSet<&str> = sessions.iter().map(|s| s.preview.as_str()).collect();
    for t in &texts {
        assert!(previews.contains(*t), "missing preview {t:?}");
    }
}

#[test]
fn test_resolve_session_supports_index_prefix_title() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let sid = new_session_id();
    let mut m = vec![msg("user", "unique title here")];
    write_session(
        dir,
        &sid,
        &mut m,
        Some(&json!({"title": "unique title here"})),
    )
    .unwrap();
    let sessions = list_sessions(dir, Some(50), 0, None);
    assert_eq!(
        resolve_session("1", &sessions),
        Some(sessions[0].id.clone())
    );
    assert_eq!(
        resolve_session(&sessions[0].id, &sessions),
        Some(sessions[0].id.clone())
    );
    let prefix: String = sessions[0].id.chars().take(18).collect();
    assert_eq!(
        resolve_session(&prefix, &sessions),
        Some(sessions[0].id.clone())
    );
    assert_eq!(
        resolve_session("unique title here", &sessions),
        Some(sid.clone())
    );
    assert_eq!(resolve_session("nope-no-such", &sessions), None);
}

#[test]
fn test_list_sessions_without_query_stops_at_limit() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    for i in 0..20 {
        let sid = format!("20250101-1300{i:02}-limit{i:02}");
        let mut m = vec![msg("user", &format!("limited {i:02}"))];
        write_session(dir, &sid, &mut m, None).unwrap();
        bump_mtime(dir, &sid, 200 + i);
    }
    let sessions = list_sessions(dir, Some(5), 0, None);
    assert_eq!(sessions.len(), 5);
}

#[test]
fn test_delete_session() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let sid = new_session_id();
    let mut m = vec![msg("user", "bye")];
    write_session(dir, &sid, &mut m, None).unwrap();
    assert!(load_session(dir, &sid).is_some());
    assert!(delete_session(dir, &sid));
    assert!(load_session(dir, &sid).is_none());
    assert!(!delete_session(dir, &sid));
}

#[test]
fn test_new_session_id_is_unique_and_sortable() {
    let mut ids = HashSet::new();
    for _ in 0..50 {
        let id = new_session_id();
        assert!(id.contains('-') && id.len() >= 15, "{}", id);
        ids.insert(id);
    }
    assert_eq!(ids.len(), 50, "session ids collided");
}

#[test]
fn test_safe_title_collapses_and_clamps() {
    assert_eq!(
        safe_title(Some("  hello\nworld  "), 60),
        Some("hello world".to_string())
    );
    let long = "x".repeat(200);
    let t = safe_title(Some(&long), 60).unwrap();
    assert_eq!(t.chars().count(), 60);
    assert!(t.ends_with('\u{2026}'));
    assert_eq!(safe_title(Some(""), 60), None);
    assert_eq!(safe_title(None, 60), None);
}

#[test]
fn test_short_id_returns_tail() {
    assert_eq!(short_id("20250101-120000-abc123"), "abc123");
    assert_eq!(short_id("plain"), "plain");
}
