use std::io::{self, Write};
use std::process::{Command, Output, Stdio};

use afi::sessions::write_session;
use serde_json::json;
use tempfile::TempDir;

/// The default env: a source that resolves but points at a dead port, so a turn
/// fails on the connection rather than on having nowhere to go.
const LIVE_SOURCE: &[(&str, &str)] = &[
    ("AFI_BASE_URL", "http://127.0.0.1:9/v1"),
    ("AFI_MODEL", "test-model"),
];

/// `AFI_ACTIVE` naming a source nobody defined - the shape a workflow takes when
/// it sets the variable and forgets the `AFI_SOURCE_*` block. Nothing is active,
/// so a turn has nowhere to go.
const NO_SOURCE: &[(&str, &str)] = &[("AFI_ACTIVE", "never-configured")];

fn run_afi(home: &TempDir, args: &[&str], input: &str) -> Output {
    run_afi_with(home, args, LIVE_SOURCE, input.as_bytes())
}

/// Run afi with an explicit env and raw stdin bytes.
///
/// Bytes rather than `&str` because one case feeds stdin something that is not
/// UTF-8 at all, which is the input the session has to refuse.
fn run_afi_with(home: &TempDir, args: &[&str], env: &[(&str, &str)], input: &[u8]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_afi"));
    command
        .args(args)
        .env_clear()
        .env("AFI_HOME", home.path())
        .env("HOME", home.path());
    for (key, value) in env {
        command.env(key, value);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("afi must start");
    // A subcommand that prints and exits can be gone before this write lands, so
    // a broken pipe means "the child already finished", not a test failure. Only
    // the empty-input `sessions` case reaches that here today, and an empty
    // `write_all` never touches the fd - so harden it before it does bite.
    let write = child.stdin.take().expect("piped stdin").write_all(input);
    match write {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {}
        Err(error) => panic!("input must write: {error}"),
    }
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
    let summary: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("summary must be valid json");
    assert_eq!(summary["ok"], false);
    // The specific sentence, not a restatement that something went wrong: the
    // reason used to live only on stderr, so a workflow reporting the JSON had
    // nothing to report.
    assert!(
        summary["error"]
            .as_str()
            .unwrap_or_default()
            .contains("can't reach"),
        "the reason must name the failure: {summary}"
    );
    // Nothing answered on port 9, which is the provider's end of the connection.
    assert_eq!(summary["error_kind"], "provider_http");
    assert_eq!(summary["model"], "test-model");
    // No turn ever reported usage, so this stays null rather than a row of zeros.
    assert_eq!(summary["usage"], serde_json::Value::Null);
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn stdout_is_nothing_but_the_summary_so_it_pipes_to_a_parser() {
    // The whole point of the flag. The rendered answer and the stats footer used
    // to share stdout with the JSON, so `afi --summary json -f p.txt | jq` died
    // with a parse error on the prose.
    let home = TempDir::new().unwrap();
    let output = run_afi(
        &home,
        &["--prompt-file", "-", "--summary", "json"],
        "hello\n",
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    serde_json::from_str::<serde_json::Value>(stdout.trim())
        .unwrap_or_else(|error| panic!("stdout must parse whole: {error}, got {stdout:?}"));
    // The human-facing error moved to stderr rather than disappearing.
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("can't reach"),
        "the error must still be reported: {stderr:?}"
    );
}

#[test]
fn a_failing_recover_command_fails_the_run() {
    // /recover runs a model turn through a different path than a plain prompt.
    // It used to discard the turn status, so a session whose only failure came
    // from /recover reported ok:true and exited 0.
    let home = TempDir::new().unwrap();
    let output = run_afi(&home, &["--summary", "json"], "/recover\n/quit\n");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let summary: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("summary must be valid json");
    assert_eq!(summary["ok"], false, "a failed /recover must fail the run");
    // The kind survives the slash-command path too, not only a plain prompt.
    assert_eq!(summary["error_kind"], "provider_http");
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn a_turn_with_no_source_to_send_it_to_fails_the_run() {
    // The turn never went anywhere. This used to print the error and exit 0 with
    // ok:true, which told a workflow the review passed.
    let home = TempDir::new().unwrap();
    let output = run_afi_with(&home, &["--summary", "json"], NO_SOURCE, b"hello\n/quit\n");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let summary: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("summary must be valid json");
    assert_eq!(summary["ok"], false);
    assert_eq!(summary["source"], serde_json::Value::Null);
    // The invocation is what was wrong, and no request was made to blame.
    assert_eq!(summary["error_kind"], "input");
    assert!(
        summary["error"]
            .as_str()
            .unwrap_or_default()
            .contains("no active source"),
        "the reason must name what is missing: {summary}"
    );
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn an_input_the_session_cannot_read_fails_the_run() {
    // A non-UTF-8 byte on stdin. Nothing was asked, so nothing was answered - and
    // this used to end the loop quietly with ok:true and exit 0.
    let home = TempDir::new().unwrap();
    let output = run_afi_with(&home, &["--summary", "json"], LIVE_SOURCE, b"\xff\xfe\n");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let summary: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("summary must be valid json");
    assert_eq!(summary["ok"], false);
    assert_eq!(summary["error_kind"], "input");
    assert!(
        summary["error"]
            .as_str()
            .unwrap_or_default()
            .contains("input error"),
        "the reason must name the failure: {summary}"
    );
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
    let summary: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("summary must be valid json");
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
