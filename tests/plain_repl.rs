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
    assert!(output.status.success());
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
