//! Port of the CLI portion of `tests/test_sessions.py`: `--resume` resolution,
//! the `sessions` paged listing/filter, page-option parsing, and rendering.

use std::collections::HashMap;
use std::fs;
use std::io::Cursor;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use afi::cli::{
    PageOptions, cli_sessions, fmt_when, print_session_list, session_id_from_args,
    session_list_page_options,
};
use afi::sessions::{SessionSummary, write_session};
use serde_json::json;

fn msg(role: &str, content: &str) -> serde_json::Value {
    json!({"role": role, "content": content})
}

/// Build an env map pointing sessions at `dir`.
fn env_for(dir: &Path) -> HashMap<String, String> {
    let mut env = HashMap::new();
    env.insert(
        "AFI_SESSIONS_DIR".to_string(),
        dir.to_string_lossy().to_string(),
    );
    env
}

/// Bump a session file's mtime so newest-first ordering is deterministic.
fn bump_mtime(dir: &Path, sid: &str, secs: i64) {
    let path = dir.join(format!("{sid}.json"));
    let f = fs::OpenOptions::new().write(true).open(&path).unwrap();
    let secs = u64::try_from(secs).unwrap_or(0);
    let times = fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::new(secs, 0));
    let _ = f.set_times(times);
}

/// Run `cli_sessions` and capture (handled, stdout) for assertions.
fn run_cli(args: &[String], env: &HashMap<String, String>) -> (bool, String) {
    let mut buf = Vec::new();
    let handled = cli_sessions(args, env, &mut buf);
    (handled, String::from_utf8(buf).unwrap())
}

/// Seed a known set: 12 `topic NN` sessions plus an auth and a css session.
fn seed_sessions(dir: &Path) {
    for i in 0..12 {
        let sid = format!("20250101-1200{i:02}-page{i:02}");
        let mut m = vec![msg("user", &format!("topic {i:02}"))];
        write_session(
            dir,
            &sid,
            &mut m,
            Some(&json!({"title": format!("topic {:02}", i)})),
        )
        .unwrap();
        bump_mtime(dir, &sid, 100 + i64::from(i));
    }
    let auth = "20250101-115000-auth00";
    let mut m = vec![msg("user", "refactor the auth module")];
    write_session(dir, auth, &mut m, Some(&json!({"title": "auth refactor"}))).unwrap();
    bump_mtime(dir, auth, 50);
    let css = "20250101-114900-css000";
    let mut m = vec![msg("user", "fix the css bug")];
    write_session(dir, css, &mut m, Some(&json!({"title": "css bugfix"}))).unwrap();
    bump_mtime(dir, css, 49);
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
fn test_cli_sessions_first_page() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let env = env_for(dir);
    seed_sessions(dir);

    // Bare `sessions` -> first page only.
    let (handled, out) = run_cli(&["sessions".to_string()], &env);
    assert!(handled);
    assert!(out.contains("20250101-120011-page11"), "{out}");
    assert!(
        out.contains("topic 02") && out.contains("topic 11"),
        "{out}"
    );
    assert!(!out.contains("topic 01"), "{out}");
    assert!(!out.contains("auth refactor"), "{out}");
    assert!(
        out.contains("resume with") && out.contains("next page"),
        "{out}"
    );
}

#[test]
fn test_cli_sessions_limit_flag() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let env = env_for(dir);
    seed_sessions(dir);

    // --sessions is the same paged listing path.
    let args = ["--sessions", "--limit", "5"].map(String::from);
    let (handled, out) = run_cli(&args, &env);
    assert!(handled);
    assert!(
        out.contains("topic 11") && out.contains("topic 07"),
        "{out}"
    );
    assert!(!out.contains("topic 06"), "{out}");
}

#[test]
fn test_cli_sessions_second_page() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let env = env_for(dir);
    seed_sessions(dir);

    // Page 2 shows the older tail.
    let args = ["sessions", "--page", "2"].map(String::from);
    let (handled, out) = run_cli(&args, &env);
    assert!(handled);
    assert!(
        out.contains("topic 01") && out.contains("auth refactor"),
        "{out}"
    );
    assert!(!out.contains("topic 11"), "{out}");
}

#[test]
fn test_cli_sessions_filter_and_no_match() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let env = env_for(dir);
    seed_sessions(dir);

    // Filter searches beyond the first page.
    let args = ["sessions", "auth"].map(String::from);
    let (handled, out) = run_cli(&args, &env);
    assert!(handled);
    assert!(
        out.contains("auth refactor") && !out.contains("css bugfix"),
        "{out}"
    );

    // No match -> graceful message, still handled.
    let args = ["sessions", "zzzznope"].map(String::from);
    let (handled, out) = run_cli(&args, &env);
    assert!(handled);
    assert!(out.contains("no sessions matching"), "{out}");
}

#[test]
fn test_cli_sessions_not_invoked_returns_false() {
    let tmp = tempfile::tempdir().unwrap();
    let env = env_for(tmp.path());
    let args = ["--resume", "1"].map(String::from);
    assert!(!run_cli(&args, &env).0);
    assert!(!cli_sessions(&[], &env, &mut Vec::new()));
}

#[test]
fn test_page_options_flags() {
    let args = ["--page", "2", "--limit", "5"].map(String::from);
    let p: PageOptions = session_list_page_options(&args);
    assert_eq!(p.page, 2);
    assert_eq!(p.limit, 5);
    assert!(p.query.is_none());
}

#[test]
fn test_page_options_query_and_fallback() {
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
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
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
