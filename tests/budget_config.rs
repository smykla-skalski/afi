//! What a cap is read from, and when a run carrying one refuses to start.
//!
//! The precedence half is ordinary. The refusal half is the point: afi caps what
//! a run spends by pricing what it used, so a budget it cannot measure is not a
//! budget - and a run that started anyway would spend real money while the
//! invocation said it was capped.

mod common;

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use afi::Runtime;
use afi::config::{Budget, FileSettings, Origin};
use serde_json::Value;
use tempfile::TempDir;

use common::summary_of;

/// Build a runtime from argv, an env, and one operator config file.
fn build(args: &[&str], env: &[(&str, &str)], file: &Path) -> Runtime {
    let args: Vec<String> = args.iter().map(ToString::to_string).collect();
    let env: HashMap<String, String> = env
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    Runtime::build_resolved(
        &args,
        env,
        &FileSettings::load(&[(file.to_path_buf(), Origin::Operator)]),
    )
}

/// The cap a runtime resolved, in whole micro-USD.
fn cap(rt: &Runtime) -> Option<u128> {
    rt.budget.map(Budget::limit)
}

#[test]
fn a_flag_beats_a_variable_beats_the_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("config.json");
    fs::write(&path, r#"{"budget_usd": 10}"#).unwrap();

    let file_only = build(&["afi"], &[], &path);
    assert_eq!(cap(&file_only), Some(10_000_000), "the file sets it");

    let with_var = build(&["afi"], &[("AFI_BUDGET_USD", "5")], &path);
    assert_eq!(cap(&with_var), Some(5_000_000), "a variable beats the file");

    let with_flag = build(
        &["afi", "--budget-usd", "2"],
        &[("AFI_BUDGET_USD", "5")],
        &path,
    );
    assert_eq!(cap(&with_flag), Some(2_000_000), "a flag beats both");
}

#[test]
fn no_cap_anywhere_is_a_run_with_no_cap() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("config.json");
    fs::write(&path, r#"{"effort": "high"}"#).unwrap();
    assert_eq!(cap(&build(&["afi"], &[], &path)), None);
}

/// Run the real binary with a clean env and no reachable endpoint.
fn run_afi(home: &TempDir, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_afi"));
    command
        .args(args)
        .env_clear()
        .env("HOME", home.path())
        .env("AFI_HOME", home.path())
        // No network: the rate table that ships is the only one in play, and the
        // refresh is off so the test never reaches for a catalogue.
        .env("AFI_PRICE_REFRESH", "0")
        .env("AFI_BASE_URL", "http://127.0.0.1:9/v1")
        .env("AFI_MODEL", "a-model-nothing-prices")
        .current_dir(home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (name, value) in env {
        command.env(name, value);
    }
    command.output().expect("afi must start")
}

#[test]
fn a_cap_afi_cannot_measure_refuses_to_start() {
    // A localhost endpoint resolves to no provider, so nothing prices its model.
    // Starting anyway would carry a cap that could never fire, which is the one
    // outcome worse than having no cap at all: it looks safe.
    let home = TempDir::new().unwrap();
    let output = run_afi(&home, &["--budget-usd", "5", "-f", "-"], &[]);

    assert_eq!(output.status.code(), Some(2), "a refusal exits 2");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--budget-usd cannot be enforced"),
        "must name the flag that was typed: {stderr}"
    );
    assert!(
        stderr.contains("a-model-nothing-prices"),
        "must name the model it could not price: {stderr}"
    );
}

#[test]
fn the_refusal_reaches_a_caller_reading_the_summary() {
    let home = TempDir::new().unwrap();
    let output = run_afi(
        &home,
        &["--budget-usd", "5", "--summary", "json", "-f", "-"],
        &[],
    );
    let summary: Value = summary_of(&output);
    assert_eq!(summary["ok"], false);
    assert_eq!(
        summary["error_kind"], "input",
        "the invocation named a cap this run cannot use: {summary}"
    );
    assert!(
        summary["error"]
            .as_str()
            .is_some_and(|why| why.contains("cannot be enforced")),
        "{summary}"
    );
    assert!(
        summary["usage"].is_null(),
        "nothing ran, so there is nothing to report about it: {summary}"
    );
}

#[test]
fn the_variable_is_named_when_the_variable_set_it() {
    // Every message quotes the spelling the operator actually used, so a fix
    // lands where the mistake is.
    let home = TempDir::new().unwrap();
    let output = run_afi(&home, &["-f", "-"], &[("AFI_BUDGET_USD", "5")]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr.contains("AFI_BUDGET_USD cannot be enforced"),
        "{stderr}"
    );
}

#[test]
fn a_priced_model_starts_normally() {
    // The counterfactual: without it, "exits 2" proves only that something went
    // wrong, not that the cap was what noticed.
    let home = TempDir::new().unwrap();
    let output = run_afi(
        &home,
        &["--budget-usd", "5", "-f", "-"],
        &[(
            "AFI_PRICES",
            r#"{"a-model-nothing-prices": {"input": 1, "output": 1}}"#,
        )],
    );
    assert_ne!(
        output.status.code(),
        Some(2),
        "a cap afi can measure must not refuse the run: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a_cap_that_is_not_an_amount_refuses_before_anything_else() {
    let home = TempDir::new().unwrap();
    let output = run_afi(&home, &["--budget-usd", "five", "-f", "-"], &[]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("is not an amount in USD"), "{stderr}");
}
