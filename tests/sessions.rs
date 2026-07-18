//! Port of `tests/test_sessions.py`. Pure file-IO: write -> load -> list ->
//! resolve -> delete, in a throwaway temp dir. No live model or terminal.

mod common;

use std::collections::HashMap;
use std::io::Cursor;

use minion::cli::{
    cli_sessions, fmt_when, print_session_list, session_id_from_args, session_list_page_options,
    PageOptions,
};
use minion::sessions::{
    delete_session, list_sessions, load_session, new_session_id, resolve_session, safe_title,
    short_id, write_session, SessionSummary,
};
use serde_json::json;

fn msg(role: &str, content: &str) -> serde_json::Value {
    json!({"role": role, "content": content})
}

/// Build an env map pointing sessions at `dir`.
fn env_for(dir: &std::path::Path) -> HashMap<String, String> {
    let mut env = HashMap::new();
    env.insert(
        "MINION_SESSIONS_DIR".to_string(),
        dir.to_string_lossy().to_string(),
    );
    env
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
    assert_eq!(loaded["schema"], "minion-rs-1");
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
        .map(|i| format!("20250101-12000{}-order{}", i, i))
        .collect();
    for (i, txt) in texts.iter().enumerate() {
        let mut m = vec![msg("user", txt)];
        write_session(dir, &sids[i], &mut m, None).unwrap();
        // Bump mtime so newest-first ordering is deterministic across runs.
        bump_mtime(dir, &sids[i], 100 + i as i64);
    }
    let sessions = list_sessions(dir, None, 0, None);
    assert_eq!(sessions[0].id, sids[2]);
    let previews: std::collections::HashSet<&str> =
        sessions.iter().map(|s| s.preview.as_str()).collect();
    for t in &texts {
        assert!(previews.contains(*t), "missing preview {:?}", t);
    }
}

fn bump_mtime(dir: &std::path::Path, sid: &str, secs: i64) {
    use std::fs;
    use std::time::Duration;
    let path = dir.join(format!("{}.json", sid));
    let f = fs::OpenOptions::new().write(true).open(&path).unwrap();
    let times =
        fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH + Duration::new(secs as u64, 0));
    let _ = f.set_times(times);
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
        let sid = format!("20250101-1300{:02}-limit{:02}", i, i);
        let mut m = vec![msg("user", &format!("limited {:02}", i))];
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
    let mut ids = std::collections::HashSet::new();
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

#[test]
fn test_bare_resume_picks_most_recent() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let env = env_for(dir);
    // No sessions yet -> bare --resume resolves to None (clean fresh start).
    assert_eq!(session_id_from_args(&["--resume".to_string()], &env), None);
    // Two sessions, newest wins.
    let old = "20250101-120000-old123";
    let mut m1 = vec![msg("user", "old")];
    write_session(dir, old, &mut m1, None).unwrap();
    bump_mtime(dir, old, 100);
    let newest = "20250101-120001-new123";
    let mut m2 = vec![msg("user", "newest")];
    write_session(dir, newest, &mut m2, None).unwrap();
    bump_mtime(dir, newest, 101);
    assert_eq!(
        session_id_from_args(&["--resume".to_string()], &env),
        Some(newest.to_string())
    );
    // --resume <n> still works.
    let resolved = session_id_from_args(&["--resume".to_string(), "1".to_string()], &env);
    assert_eq!(resolved.as_deref(), Some(newest));
}

#[test]
fn test_resume_flag_without_target_ignores_following_dash_flags() {
    let tmp = tempfile::tempdir().unwrap();
    let env = env_for(tmp.path());
    let resolved = session_id_from_args(&["--resume".to_string(), "--yolo".to_string()], &env);
    assert_ne!(resolved.as_deref(), Some("--yolo"));
}

#[test]
fn test_cli_sessions_list_and_filter() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let env = env_for(dir);

    // Seed a known set.
    for i in 0..12 {
        let sid = format!("20250101-1200{:02}-page{:02}", i, i);
        let mut m = vec![msg("user", &format!("topic {:02}", i))];
        write_session(
            dir,
            &sid,
            &mut m,
            Some(&json!({"title": format!("topic {:02}", i)})),
        )
        .unwrap();
        bump_mtime(dir, &sid, 100 + i as i64);
    }
    let auth = "20250101-115000-auth00";
    let mut m = vec![msg("user", "refactor the auth module")];
    write_session(dir, auth, &mut m, Some(&json!({"title": "auth refactor"}))).unwrap();
    bump_mtime(dir, auth, 50);
    let css = "20250101-114900-css000";
    let mut m = vec![msg("user", "fix the css bug")];
    write_session(dir, css, &mut m, Some(&json!({"title": "css bugfix"}))).unwrap();
    bump_mtime(dir, css, 49);

    // Bare `sessions` -> first page only.
    let mut buf = Vec::new();
    let handled = cli_sessions(&["sessions".to_string()], &env, &mut buf);
    assert!(handled);
    let out = String::from_utf8(buf).unwrap();
    assert!(out.contains("20250101-120011-page11"), "{}", out);
    assert!(out.contains("topic 02"), "{}", out);
    assert!(out.contains("topic 11"), "{}", out);
    assert!(!out.contains("topic 01"), "{}", out);
    assert!(!out.contains("auth refactor"), "{}", out);
    assert!(out.contains("resume with"), "{}", out);
    assert!(out.contains("next page"), "{}", out);

    // --sessions is the same paged listing path.
    let mut buf = Vec::new();
    let handled = cli_sessions(
        &[
            "--sessions".to_string(),
            "--limit".to_string(),
            "5".to_string(),
        ],
        &env,
        &mut buf,
    );
    assert!(handled);
    let out = String::from_utf8(buf).unwrap();
    assert!(out.contains("topic 11"), "{}", out);
    assert!(out.contains("topic 07"), "{}", out);
    assert!(!out.contains("topic 06"), "{}", out);

    // Page 2 shows the older tail.
    let mut buf = Vec::new();
    let handled = cli_sessions(
        &[
            "sessions".to_string(),
            "--page".to_string(),
            "2".to_string(),
        ],
        &env,
        &mut buf,
    );
    assert!(handled);
    let out = String::from_utf8(buf).unwrap();
    assert!(out.contains("topic 01"), "{}", out);
    assert!(out.contains("auth refactor"), "{}", out);
    assert!(!out.contains("topic 11"), "{}", out);

    // Filter searches beyond the first page.
    let mut buf = Vec::new();
    let handled = cli_sessions(
        &["sessions".to_string(), "auth".to_string()],
        &env,
        &mut buf,
    );
    assert!(handled);
    let out = String::from_utf8(buf).unwrap();
    assert!(out.contains("auth refactor"), "{}", out);
    assert!(!out.contains("css bugfix"), "{}", out);

    // No match -> graceful message, still handled.
    let mut buf = Vec::new();
    let handled = cli_sessions(
        &["sessions".to_string(), "zzzznope".to_string()],
        &env,
        &mut buf,
    );
    assert!(handled);
    assert!(String::from_utf8(buf)
        .unwrap()
        .contains("no sessions matching"));

    // Not a sessions invocation -> returns false.
    assert!(!cli_sessions(
        &["--resume".to_string(), "1".to_string()],
        &env,
        &mut Vec::new(),
    ));
    assert!(!cli_sessions(&[], &env, &mut Vec::new()));
}

#[test]
fn test_page_options_parse_flags() {
    let p: PageOptions = session_list_page_options(&[
        "--page".to_string(),
        "2".to_string(),
        "--limit".to_string(),
        "5".to_string(),
    ]);
    assert_eq!(p.page, 2);
    assert_eq!(p.limit, 5);
    assert!(p.query.is_none());

    let p = session_list_page_options(&["refactor".to_string(), "auth".to_string()]);
    assert_eq!(p.query.as_deref(), Some("refactor auth"));

    let p = session_list_page_options(&["--page=3".to_string()]);
    assert_eq!(p.page, 3);

    // Invalid page falls back to default.
    let p = session_list_page_options(&["--page".to_string(), "nope".to_string()]);
    assert_eq!(p.page, 1);
    assert!(!p.warnings.is_empty());
}

#[test]
fn test_fmt_when_relative() {
    let now = chrono::Local::now().timestamp() as f64;
    assert_eq!(fmt_when(0.0), "?");
    // Recent.
    assert_eq!(fmt_when(now - 10.0), "just now");
    assert!(fmt_when(now - 300.0).contains("m ago"));
    assert!(fmt_when(now - 7200.0).contains("h ago"));
}

#[test]
fn test_print_session_list_renders_title_and_meta() {
    let s = SessionSummary {
        id: "20250101-120000-abc123".to_string(),
        short: "abc123".to_string(),
        title: "hello world".to_string(),
        description: None,
        preview: "hello world".to_string(),
        updated_at: 0.0,
        n: 3,
        model: Some("glm-5.2".to_string()),
        source: Some("zai".to_string()),
        cwd: Some("/tmp/proj".to_string()),
    };
    let mut buf = Vec::new();
    let mut cursor = Cursor::new(&mut buf);
    print_session_list(&mut cursor, &[s], 1, None);
    let out = String::from_utf8(buf).unwrap();
    assert!(out.contains("abc123"));
    assert!(out.contains("hello world"));
    assert!(out.contains("3 msg"));
    assert!(out.contains("source zai"));
    assert!(out.contains("model glm-5.2"));
}

// Silence unused imports in `common` if a later phase hasn't wired them yet.
#[allow(dead_code)]
fn _silence() {
    let _ = common::build(&["minion"], &[]);
    let _ = common::build_with_env_file(&["minion"], &[], None);
}
