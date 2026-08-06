//! `--summary-file` end to end: a real `afi` process, a real path, and what
//! lands on each of the three channels a caller can read.
//!
//! Every run here points at a closed port, so every run fails. That is the
//! interesting case for a report: a workflow that only gets its summary when the
//! model answers cannot tell a broken run from a missing one.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use serde_json::Value;
use tempfile::TempDir;

fn run_afi(home: &TempDir, args: &[&str], env: &[(&str, &str)], input: &str) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_afi"));
    command
        .args(args)
        .env_clear()
        .env("AFI_HOME", home.path())
        .env("HOME", home.path())
        .env("AFI_BASE_URL", "http://127.0.0.1:9/v1")
        .env("AFI_MODEL", "test-model")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in env {
        command.env(key, value);
    }
    let mut child = command.spawn().expect("afi must start");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(input.as_bytes())
        .expect("the prompt must write");
    child.wait_with_output().expect("afi must exit")
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout must be utf-8")
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("stderr must be utf-8")
}

fn read_summary(path: &Path) -> Value {
    let body = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("the summary file must exist: {error}"));
    serde_json::from_str(&body)
        .unwrap_or_else(|error| panic!("the summary file must parse whole: {error}, got {body:?}"))
}

#[test]
fn the_summary_lands_on_the_path_and_stdout_keeps_the_human_view() {
    // The whole point of the flag. Capturing stdout to get the JSON costs the
    // readable rendering, so a workflow that wants both ends up printing the
    // run twice - once as the copy it read back out of the file.
    let home = TempDir::new().unwrap();
    let path = home.path().join("run.json");
    let output = run_afi(
        &home,
        &["--summary-file", path.to_str().unwrap()],
        &[],
        "hello\n/quit\n",
    );

    let summary = read_summary(&path);
    assert_eq!(summary["ok"], false);
    assert_eq!(summary["model"], "test-model");
    assert!(!summary["error"].is_null(), "a failure must name a reason");

    let stdout = stdout_of(&output);
    assert!(
        !stdout.lines().any(|line| line.starts_with('{')),
        "the file was asked for, not a second copy on stdout: {stdout:?}"
    );
    assert!(
        stdout.contains("resume with: afi --resume"),
        "human output must stay on stdout: {stdout:?}"
    );
}

#[test]
fn the_env_var_does_the_same_and_the_flag_wins_over_it() {
    let home = TempDir::new().unwrap();
    let from_env = home.path().join("env.json");
    let from_flag = home.path().join("flag.json");

    let output = run_afi(
        &home,
        &["--prompt-file", "-"],
        &[("AFI_SUMMARY_FILE", from_env.to_str().unwrap())],
        "hello\n",
    );
    assert_eq!(read_summary(&from_env)["model"], "test-model");
    assert_eq!(output.status.code(), Some(1), "the run itself still failed");

    run_afi(
        &home,
        &[
            "--prompt-file",
            "-",
            "--summary-file",
            from_flag.to_str().unwrap(),
        ],
        &[("AFI_SUMMARY_FILE", from_env.to_str().unwrap())],
        "hello\n",
    );
    assert!(from_flag.exists(), "the flag must win over the variable");
}

#[test]
fn a_blank_variable_names_no_file_and_is_not_an_error() {
    // `AFI_SUMMARY_FILE=""` is what an exported-but-unset shell variable looks
    // like, and it reads as off here the same way every other variable does.
    // Exit 1 is the closed port, not a refusal - exit 2 would mean refused.
    let home = TempDir::new().unwrap();
    let output = run_afi(
        &home,
        &["--prompt-file", "-"],
        &[("AFI_SUMMARY_FILE", "")],
        "hi\n",
    );
    assert_eq!(output.status.code(), Some(1), "{}", stderr_of(&output));
    assert!(!stderr_of(&output).contains("needs a value"));
}

#[test]
fn a_blank_flag_value_is_refused_rather_than_read_as_no_file() {
    // `afi --summary-file "$OUT"` with `OUT` unset. The quoted form is how a CI
    // script is written, and it reaches argv as an empty argument rather than as
    // no argument, so the absent-value check alone does not see it. Accepting it
    // exits 0 having written nothing to the path the next step reads - or leaves
    // a file from an earlier run standing as this run's result.
    let home = TempDir::new().unwrap();
    for blank in ["", "   "] {
        let output = run_afi(&home, &["--summary-file", blank], &[], "/quit\n");
        assert_eq!(
            output.status.code(),
            Some(2),
            "{blank:?} must not start a run: {}",
            stderr_of(&output)
        );
        assert!(stderr_of(&output).contains("--summary-file needs a value"));
    }
}

#[test]
fn asking_for_both_gets_both_and_they_describe_one_run() {
    let home = TempDir::new().unwrap();
    let path = home.path().join("run.json");
    let output = run_afi(
        &home,
        &[
            "--prompt-file",
            "-",
            "--summary",
            "json",
            "--summary-file",
            path.to_str().unwrap(),
        ],
        &[],
        "hello\n",
    );

    let stdout = stdout_of(&output);
    let printed: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|error| panic!("stdout must parse whole: {error}, got {stdout:?}"));
    // Built once and rendered twice, so the file and the pipe cannot disagree
    // about what the run did.
    assert_eq!(printed, read_summary(&path));
}

#[test]
fn a_path_that_cannot_be_written_stops_the_run_before_it_is_paid_for() {
    // Falling back to stdout would be no fallback: a caller that named a path is
    // not watching stdout for the JSON. Failing after the run would be worse
    // still - the tokens are spent by then and the answer has nowhere to go.
    let home = TempDir::new().unwrap();
    let path = home.path().join("no-such-dir/run.json");
    let output = run_afi(
        &home,
        &[
            "--prompt-file",
            "-",
            "--summary-file",
            path.to_str().unwrap(),
        ],
        &[],
        "hello\n",
    );

    assert_eq!(output.status.code(), Some(2), "the run must not start");
    let stderr = stderr_of(&output);
    assert!(stderr.contains("no-such-dir/run.json"), "{stderr}");
    assert!(
        !stderr.contains("can't reach"),
        "the model was called anyway: {stderr}"
    );
    // A tool name registry answers a mistyped tool, and nothing about a path.
    assert!(!stderr.contains("known tools:"), "{stderr}");
}

#[test]
fn a_directory_in_place_of_the_file_is_refused_too() {
    let home = TempDir::new().unwrap();
    let output = run_afi(
        &home,
        &[
            "--prompt-file",
            "-",
            "--summary-file",
            home.path().to_str().unwrap(),
        ],
        &[],
        "hello\n",
    );
    assert_eq!(output.status.code(), Some(2), "{}", stderr_of(&output));
    assert!(stderr_of(&output).contains("is a directory"));
}

#[test]
fn a_trailing_slash_is_refused_before_the_run_too() {
    // `"$OUTDIR/$NAME"` with `NAME` unset. It used to pass the startup probe -
    // the temp file is an ordinary sibling of the parent - and fail only at the
    // rename, after the whole run had been paid for.
    let home = TempDir::new().unwrap();
    let path = home.path().join("out/");
    let output = run_afi(
        &home,
        &[
            "--prompt-file",
            "-",
            "--summary-file",
            path.to_str().unwrap(),
        ],
        &[],
        "hello\n",
    );

    assert_eq!(output.status.code(), Some(2), "the run must not start");
    let stderr = stderr_of(&output);
    assert!(stderr.contains("names a directory"), "{stderr}");
    assert!(
        !stderr.contains("can't reach"),
        "the model was called anyway: {stderr}"
    );
}

#[test]
fn the_flag_with_no_value_is_refused_rather_than_ignored() {
    // `afi --summary-file $OUT` with `OUT` unset would otherwise exit 0 having
    // written nothing to the path the next workflow step is about to read.
    let home = TempDir::new().unwrap();
    let output = run_afi(&home, &["--summary-file"], &[], "/quit\n");
    assert_eq!(output.status.code(), Some(2), "the run must not start");
    assert!(stderr_of(&output).contains("--summary-file needs a value"));
}

#[test]
fn a_value_that_is_another_flag_is_refused_rather_than_swallowed() {
    let home = TempDir::new().unwrap();
    let output = run_afi(&home, &["--summary-file", "--yolo"], &[], "/quit\n");
    assert_eq!(output.status.code(), Some(2), "the run must not start");
    assert!(stderr_of(&output).contains("--summary-file needs a value"));
}
