use std::io::Write;
use std::process::{Command, Output, Stdio};

use afi::sessions::write_session;
use serde_json::json;
use tempfile::TempDir;

fn run_afi(home: &TempDir, args: &[&str], input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_afi"))
        .args(args)
        .env_clear()
        .env("AFI_HOME", home.path())
        .env("AFI_BASE_URL", "http://127.0.0.1:9/v1")
        .env("AFI_MODEL", "test-model")
        .env("HOME", home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("afi must start");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(input.as_bytes())
        .expect("input must write");
    child.wait_with_output().expect("afi must exit")
}

#[test]
fn piped_repl_has_no_prompt_or_terminal_escapes() {
    let home = TempDir::new().unwrap();
    let output = run_afi(&home, &[], "/quit\n");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!stdout.contains("  > "));
    assert!(!stdout.contains('\x1b'));
    assert!(!stderr.contains('\x1b'));
    assert!(stdout.contains("resume with: afi --resume"));
}

#[test]
fn stdin_prompt_file_stays_plain_and_session_free() {
    let home = TempDir::new().unwrap();
    let output = run_afi(&home, &["--prompt-file", "-"], "hello\n");
    assert!(!output.stdout.contains(&b'\x1b'));
    assert!(!output.stderr.contains(&b'\x1b'));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("can't reach"),
        "unexpected stderr: {stderr:?}"
    );
    assert!(!home.path().join("sessions").exists());
}

#[test]
fn a_one_shot_run_against_an_unreachable_server_exits_nonzero() {
    // The base url points at a closed port, so this run cannot succeed. It used
    // to exit 0 regardless, which let CI read a failed review as a passing one.
    let home = TempDir::new().unwrap();
    let output = run_afi(&home, &["--prompt-file", "-"], "hello\n");
    assert_eq!(
        output.status.code(),
        Some(1),
        "a failed run must exit 1: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn the_json_summary_reports_a_failed_run_as_not_ok() {
    let home = TempDir::new().unwrap();
    let output = run_afi(
        &home,
        &["--prompt-file", "-", "--summary", "json"],
        "hello\n",
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let line = stdout
        .lines()
        .rev()
        .find(|l| l.starts_with('{'))
        .unwrap_or_else(|| panic!("no json summary in stdout: {stdout:?}"));
    let summary: serde_json::Value =
        serde_json::from_str(line).expect("summary must be valid json");
    assert_eq!(summary["ok"], false);
    assert!(!summary["error"].is_null(), "a failure must name a reason");
    assert_eq!(summary["model"], "test-model");
    // No turn ever reported usage, so this stays null rather than a row of zeros.
    assert_eq!(summary["usage"], serde_json::Value::Null);
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn no_summary_is_printed_unless_asked_for() {
    let home = TempDir::new().unwrap();
    let output = run_afi(&home, &["--prompt-file", "-"], "hello\n");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        !stdout.lines().any(|l| l.starts_with('{')),
        "unexpected summary on stdout: {stdout:?}"
    );
}

#[test]
fn a_piped_session_without_a_prompt_file_also_reports_and_fails() {
    // Piped stdin with no --prompt-file is the other non-interactive entry
    // point. It used to print no summary and exit 0 no matter what happened,
    // so a workflow using it could not tell a broken run from a good one.
    let home = TempDir::new().unwrap();
    let output = run_afi(&home, &["--summary", "json"], "hello\n/quit\n");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let line = stdout
        .lines()
        .rev()
        .find(|l| l.starts_with('{'))
        .unwrap_or_else(|| panic!("no json summary in stdout: {stdout:?}"));
    let summary: serde_json::Value =
        serde_json::from_str(line).expect("summary must be valid json");
    assert_eq!(summary["ok"], false);
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn a_piped_session_prints_no_summary_by_default() {
    let home = TempDir::new().unwrap();
    let output = run_afi(&home, &[], "hello\n/quit\n");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        !stdout.lines().any(|l| l.starts_with('{')),
        "unexpected summary on stdout: {stdout:?}"
    );
}

#[test]
fn piped_sessions_listing_has_no_terminal_escapes() {
    let home = TempDir::new().unwrap();
    let session_id = "20250101-120000-piped0";
    let mut messages = vec![json!({"role": "user", "content": "plain listing"})];
    write_session(
        &home.path().join("sessions"),
        session_id,
        &mut messages,
        Some(&json!({"title": "plain listing"})),
    )
    .unwrap();

    let output = run_afi(&home, &["sessions"], "");
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains(session_id));
    assert!(!output.stdout.contains(&b'\x1b'));
    assert!(!output.stderr.contains(&b'\x1b'));
}
