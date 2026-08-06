//! The tool policy as a user sees it: the two flags, the two env vars, which
//! wins, and what a bad policy does to the process.

mod common;

use std::io::{self, Write};
use std::process::{Command, Output, Stdio};

use afi::model::ModelConfig;
use afi::repl::banner;
use tempfile::TempDir;

/// Run the real binary with a clean env so nothing leaks in from the shell.
fn run_afi(home: &TempDir, args: &[&str], env: &[(&str, &str)], input: &str) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_afi"));
    command
        .args(args)
        .env_clear()
        .env("AFI_HOME", home.path())
        .env("AFI_BASE_URL", "http://127.0.0.1:9/v1")
        .env("AFI_MODEL", "test-model")
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
    // A run that refuses to start exits before reading stdin, so the write races
    // the pipe closing. Losing that race is the behaviour under test, not a
    // failure - every assertion here is on the exit code and stderr. Treating a
    // broken pipe as fatal made these tests pass on a fast machine and fail on a
    // CI runner.
    let write = child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(input.as_bytes());
    match write {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {}
        Err(error) => panic!("input must write: {error}"),
    }
    child.wait_with_output().expect("afi must exit")
}

// --- resolution ----------------------------------------------------------------

#[test]
fn no_flag_and_no_env_permits_every_tool() {
    let rt = common::build(&["afi"], &[]);
    assert!(rt.tool_policy.is_unrestricted());
    assert_eq!(rt.tool_policy.describe(), "all");
}

#[test]
fn the_allowed_tools_flag_narrows_the_run() {
    let rt = common::build(&["afi", "--allowed-tools", "read_file,list_dir"], &[]);
    assert_eq!(rt.tool_policy.permitted(), ["read_file", "list_dir"]);
}

#[test]
fn the_disallowed_tools_flag_removes_from_the_full_set() {
    let rt = common::build(&["afi", "--disallowed-tools", "write_file,edit_file"], &[]);
    assert_eq!(
        rt.tool_policy.permitted(),
        ["read_file", "list_dir", "run_bash", "wait_background"]
    );
}

#[test]
fn the_env_vars_work_on_their_own() {
    let rt = common::build(
        &["afi"],
        &[
            ("AFI_ALLOWED_TOOLS", "read_file,run_bash"),
            ("AFI_DISALLOWED_TOOLS", "run_bash"),
        ],
    );
    assert_eq!(rt.tool_policy.permitted(), ["read_file"]);
}

#[test]
fn the_flag_wins_over_the_env_var() {
    // Same precedence as every other setting: a workflow can tighten a policy on
    // one step without editing the job-wide env.
    let rt = common::build(
        &["afi", "--allowed-tools", "read_file"],
        &[("AFI_ALLOWED_TOOLS", "read_file,write_file,run_bash")],
    );
    assert_eq!(rt.tool_policy.permitted(), ["read_file"]);
}

#[test]
fn each_flag_overrides_only_its_own_variable() {
    let rt = common::build(
        &["afi", "--disallowed-tools", "run_bash"],
        &[
            ("AFI_ALLOWED_TOOLS", "read_file,run_bash"),
            ("AFI_DISALLOWED_TOOLS", "read_file"),
        ],
    );
    assert_eq!(rt.tool_policy.permitted(), ["read_file"]);
}

#[test]
fn a_blank_value_is_not_a_lockout() {
    // An unset shell variable expands to `""` in a workflow, which must mean
    // "no policy" rather than "no tools".
    let rt = common::build(
        &["afi", "--allowed-tools", ""],
        &[("AFI_DISALLOWED_TOOLS", "")],
    );
    assert!(rt.tool_policy.is_unrestricted());
}

#[test]
fn the_model_config_agrees_with_the_runtime() {
    // Two parses of the same env. They must not be able to disagree, or the
    // banner would advertise a policy the dispatcher does not enforce.
    let rt = common::build(&["afi", "--allowed-tools", "read_file,list_dir"], &[]);
    let config = ModelConfig::from_env(&rt.env);
    assert_eq!(config.tool_policy, rt.tool_policy);
}

// --- the banner ----------------------------------------------------------------

#[test]
fn the_banner_names_the_policy_only_when_one_applies() {
    let plain = common::build(&["afi"], &[]);
    assert!(!banner(&plain).contains("tools:"));

    let restricted = common::build(&["afi", "--allowed-tools", "read_file"], &[]);
    assert!(banner(&restricted).contains("tools:read_file"));
}

// --- the process ---------------------------------------------------------------

#[test]
fn an_unknown_tool_name_refuses_to_start() {
    // The dangerous typo. `run_bsah` in a deny list matches nothing, so a run
    // that started would be unrestricted while looking restricted.
    let home = TempDir::new().unwrap();
    let output = run_afi(&home, &["--disallowed-tools", "run_bsah"], &[], "/quit\n");
    assert_eq!(output.status.code(), Some(2), "must not start");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("run_bsah"), "{stderr}");
    assert!(stderr.contains("known tools:"), "{stderr}");
}

#[test]
fn an_unknown_name_in_the_env_var_also_refuses_to_start() {
    let home = TempDir::new().unwrap();
    let output = run_afi(
        &home,
        &[],
        &[("AFI_ALLOWED_TOOLS", "read_file,reed_file")],
        "/quit\n",
    );
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("reed_file"), "{stderr}");
    // Naming only the flags would send someone debugging a CI failure hunting
    // for a flag that was never passed.
    assert!(stderr.contains("AFI_ALLOWED_TOOLS"), "{stderr}");
}

#[test]
fn a_policy_flag_with_no_value_refuses_to_start() {
    // The fail-open. `afi --yolo -f task.txt --disallowed-tools $DENY` with DENY
    // unset ends argv at the flag; resolving that to "no policy" would grant
    // write_file, edit_file, and run_bash while the command line said otherwise,
    // and one-shot mode prints no banner, so there would be no signal at all.
    let home = TempDir::new().unwrap();
    for flag in ["--allowed-tools", "--disallowed-tools"] {
        let output = run_afi(&home, &[flag], &[], "/quit\n");
        assert_eq!(output.status.code(), Some(2), "{flag} must not start");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(
            stderr.contains(&format!("{flag} needs a value")),
            "{stderr}"
        );
    }
}

#[test]
fn a_policy_flag_swallowing_the_next_flag_refuses_to_start() {
    let home = TempDir::new().unwrap();
    let output = run_afi(&home, &["--disallowed-tools", "--yolo"], &[], "/quit\n");
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("--disallowed-tools needs a value"),
        "{stderr}"
    );
}

#[test]
fn a_missing_value_is_recorded_rather_than_silently_widening_the_policy() {
    let rt = common::build(&["afi", "--disallowed-tools"], &[]);
    assert_eq!(rt.flag_errors, ["--disallowed-tools needs a value"]);
    assert!(!rt.refusals().is_empty(), "the run must be refused");
}

#[test]
fn a_valid_policy_starts_normally() {
    let home = TempDir::new().unwrap();
    let output = run_afi(
        &home,
        &["--allowed-tools", "read_file,list_dir"],
        &[],
        "/quit\n",
    );
    assert!(output.status.success(), "{:?}", output.status);
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("tools:read_file,list_dir"), "{stdout}");
}

#[test]
fn the_json_summary_records_what_the_run_could_call() {
    // An auditor reading a CI log should not have to trust that the workflow
    // passed the flag it claims to.
    let home = TempDir::new().unwrap();
    let output = run_afi(
        &home,
        &[
            "--summary",
            "json",
            "--allowed-tools",
            "read_file",
            "-f",
            "-",
        ],
        &[],
        "say hi\n",
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let summary: serde_json::Value =
        serde_json::from_str(stdout.trim()).unwrap_or_else(|error| panic!("{error}: {stdout}"));
    assert_eq!(summary["tools"], serde_json::json!(["read_file"]));
}
