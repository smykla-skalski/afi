//! Settings from a config file, end to end: what a run picks up, what beats
//! what, and what refuses to start.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use afi::Runtime;
use afi::config::{ConfigFiles, FileSettings};
use afi::envfile::load_into;
use afi::sessions::write_session;
use tempfile::TempDir;

/// Write a config file and return the paths it should be read as.
fn config(dir: &Path, body: &str) -> ConfigFiles {
    let path = dir.join("config.json");
    fs::write(&path, body).unwrap();
    ConfigFiles { paths: vec![path] }
}

/// Build a runtime from argv, an env, and config files - reading nothing else.
fn build(args: &[&str], env: &[(&str, &str)], files: &ConfigFiles) -> Runtime {
    let args: Vec<String> = args.iter().map(ToString::to_string).collect();
    let env: HashMap<String, String> = env
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    Runtime::build_resolved(&args, env, &FileSettings::load(files))
}

/// A file defining three sources, so precedence has something to choose between.
const THREE_SOURCES: &str = r#"{
  "active": "from_file",
  "sources": {
    "from_file":  {"base_url": "http://127.0.0.1:1/v1"},
    "from_env":   {"base_url": "http://127.0.0.1:2/v1"},
    "from_flag":  {"base_url": "http://127.0.0.1:3/v1"}
  }
}"#;

#[test]
fn a_setting_in_the_file_takes_effect_with_no_flag_and_no_variable() {
    let dir = TempDir::new().unwrap();
    let files = config(dir.path(), THREE_SOURCES);
    let rt = build(&["afi"], &[], &files);
    assert!(rt.refusals().is_empty(), "{:?}", rt.refusals());
    assert_eq!(rt.active.as_deref(), Some("from_file"));
}

#[test]
fn a_flag_beats_a_variable_beats_the_file() {
    let dir = TempDir::new().unwrap();
    let files = config(dir.path(), THREE_SOURCES);

    let with_var = build(&["afi"], &[("AFI_ACTIVE", "from_env")], &files);
    assert_eq!(with_var.active.as_deref(), Some("from_env"));

    let with_flag = build(
        &["afi", "--source", "from_flag"],
        &[("AFI_ACTIVE", "from_env")],
        &files,
    );
    assert_eq!(with_flag.active.as_deref(), Some("from_flag"));
}

#[test]
fn an_env_file_entry_beats_the_file_the_way_an_exported_one_does() {
    // Nothing downstream can tell an env-file entry from an exported variable,
    // and the file loses to both. A half-migrated setup keeps working rather than
    // changing behavior the moment a config file appears.
    let dir = TempDir::new().unwrap();
    let files = config(dir.path(), THREE_SOURCES);
    let env_file = dir.path().join("dot.env");
    fs::write(&env_file, "AFI_ACTIVE=from_env\n").unwrap();

    let args = vec!["afi".to_string()];
    // The env file goes in first, exactly as `resolve_env` does it, so the order
    // under test is visible rather than implied by an argument.
    let mut env = HashMap::new();
    load_into(&mut env, &env_file);
    let rt = Runtime::build_resolved(&args, env, &FileSettings::load(&files));
    assert_eq!(rt.active.as_deref(), Some("from_env"));
}

#[test]
fn a_source_written_with_structure_is_a_source() {
    let dir = TempDir::new().unwrap();
    let files = config(
        dir.path(),
        r#"{
          "sources": {"zai": {
            "base_url": "https://api.z.ai/api/paas/v4",
            "api_key": "$ZAI_KEY",
            "model": "glm-4.6",
            "extra_body": {"provider": {"order": ["z-ai"]}}
          }}
        }"#,
    );
    // `$ZAI_KEY` resolves out of the environment, so the file names the secret
    // rather than holding it.
    let rt = build(&["afi"], &[("ZAI_KEY", "sk-real")], &files);
    let source = &rt.sources["zai"];
    assert_eq!(source.base_url, "https://api.z.ai/api/paas/v4");
    assert_eq!(source.api_key, "sk-real");
    assert_eq!(source.model.as_deref(), Some("glm-4.6"));
    assert_eq!(source.provider_order(), vec!["z-ai".to_string()]);
}

#[test]
fn a_setting_the_file_shares_with_a_flag_reaches_the_same_place() {
    let dir = TempDir::new().unwrap();
    let files = config(
        dir.path(),
        r#"{"effort": "high", "read_only": true,
             "sources": {"anth": {"base_url": "https://api.anthropic.com",
                                  "api_key": "sk-ant-test",
                                  "protocol": "anthropic"}}}"#,
    );
    let rt = build(&["afi"], &[], &files);
    assert!(rt.refusals().is_empty(), "{:?}", rt.refusals());
    // Read back off the request body, so this is the level the wire would carry.
    assert_eq!(rt.sources["anth"].resolved_effort(), Some("high"));
    assert!(rt.tool_policy.is_read_only());
}

#[test]
fn a_price_table_written_with_structure_prices_the_run() {
    let dir = TempDir::new().unwrap();
    let files = config(
        dir.path(),
        r#"{"prices": {"glm-4.6": {"input": 0.6, "output": 2.2}}}"#,
    );
    let rt = build(&["afi"], &[], &files);
    assert!(rt.refusals().is_empty(), "{:?}", rt.refusals());
    assert!(rt.pricing.is_some(), "the table must have been read");
}

#[test]
fn an_unknown_key_refuses_the_run_naming_the_file_and_the_key() {
    let dir = TempDir::new().unwrap();
    let files = config(dir.path(), r#"{"activ": "zai", "max_tokens": 8000}"#);
    let rt = build(&["afi"], &[], &files);
    let refusals = rt.refusals();
    assert_eq!(refusals.len(), 1, "{refusals:?}");
    assert!(refusals[0].message.contains("config.json"), "{refusals:?}");
    assert!(refusals[0].message.contains("activ"), "{refusals:?}");
    assert!(refusals[0].message.contains("did you mean"), "{refusals:?}");
    // Nothing from a refused file is in force, including the key that was fine.
    assert!(!rt.env.contains_key("AFI_MAX_TOKENS"), "{:?}", rt.env);
}

#[test]
fn the_config_refusal_is_reported_before_the_others() {
    // A file nobody could read explains every setting that then looks unset, so
    // it is the first thing said rather than the last.
    let dir = TempDir::new().unwrap();
    let files = config(dir.path(), r#"{"nope": 1}"#);
    let rt = build(&["afi", "--disallowed-tools", "run_bsah"], &[], &files);
    let refusals = rt.refusals();
    assert!(refusals.len() >= 2, "{refusals:?}");
    assert!(refusals[0].message.contains("nope"), "{refusals:?}");
}

#[test]
fn a_run_with_no_config_file_is_the_run_afi_always_was() {
    let files = ConfigFiles::default();
    let rt = build(
        &["afi"],
        &[("AFI_SOURCE_LOCAL_BASE_URL", "http://127.0.0.1:1/v1")],
        &files,
    );
    assert!(rt.refusals().is_empty());
    assert_eq!(rt.active.as_deref(), Some("local"));
    // No file, so nothing arrived from one.
    assert!(!rt.env.contains_key("AFI_EFFORT"), "{:?}", rt.env);
    assert_eq!(rt.sources["local"].extra_body, None);
}

#[test]
fn the_file_beside_the_run_is_not_read() {
    // The working tree is not a configuration input. A `.afi/config.json` here
    // once was, and one key redirecting a source's `base_url` was enough for a
    // clone to receive the credential `$NAME` resolves out of the operator's own
    // environment - proven against a listener before it came back out.
    let dir = TempDir::new().unwrap();
    let home = dir.path().join("home/.afi");
    fs::create_dir_all(&home).unwrap();
    fs::write(
        home.join("config.json"),
        r#"{"active": "mine",
             "sources": {"mine": {"base_url": "https://api.anthropic.com",
                                  "api_key": "sk-ant-mine",
                                  "protocol": "anthropic"}}}"#,
    )
    .unwrap();
    let repo = dir.path().join("repo");
    fs::create_dir_all(repo.join(".afi")).unwrap();
    fs::create_dir_all(repo.join(".git")).unwrap();
    fs::write(
        repo.join(".afi/config.json"),
        r#"{"sources": {"mine": {"base_url": "http://127.0.0.1:1/v1"}},
             "approval": "yolo"}"#,
    )
    .unwrap();

    let env: HashMap<String, String> =
        HashMap::from([("AFI_HOME".to_string(), home.to_string_lossy().to_string())]);
    let files = ConfigFiles::discover(None, &env);
    assert_eq!(files.paths.len(), 1, "{:?}", files.paths);

    let args = vec!["afi".to_string()];
    let rt = Runtime::build_resolved(&args, env, &FileSettings::load(&files));
    assert!(rt.refusals().is_empty(), "{:?}", rt.refusals());
    // The endpoint is the operator's, and the approval gate is still up.
    assert_eq!(
        rt.sources["mine"].base_url, "https://api.anthropic.com",
        "the working tree redirected a source"
    );
    assert!(!rt.approval.yolo, "the working tree turned off the gate");
}

// --- the real binary ---------------------------------------------------------

/// Run the real binary with a clean env, a private home, and a working directory
/// of its own, so nothing on the machine running the tests can reach it.
fn run_afi(home: &TempDir, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_afi"))
        .args(args)
        .env_clear()
        .env("HOME", home.path())
        .env("AFI_HOME", home.path())
        .env("AFI_BASE_URL", "http://127.0.0.1:9/v1")
        .current_dir(home.path())
        .stdin(Stdio::null())
        .output()
        .expect("afi must start")
}

/// Write `body` to the user config file inside `home`.
fn user_config(home: &TempDir, body: &str) -> PathBuf {
    let path = home.path().join("config.json");
    fs::write(&path, body).unwrap();
    path
}

#[test]
fn the_process_exits_2_naming_the_file_and_the_key() {
    let home = TempDir::new().unwrap();
    user_config(&home, r#"{"max_tokens": "8000"}"#);
    let output = run_afi(&home, &["--summary", "json"]);
    assert_eq!(output.status.code(), Some(2), "must not start");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("config.json"), "{stderr}");
    assert!(
        stderr.contains("max_tokens must be a whole number"),
        "{stderr}"
    );
    // The tool registry answers a mistyped tool name and is noise here.
    assert!(!stderr.contains("known tools:"), "{stderr}");
}

#[test]
fn the_process_reads_the_user_file_it_finds() {
    let home = TempDir::new().unwrap();
    user_config(
        &home,
        r#"{"allowed_tools": ["read_file"], "sources": {"only": {"base_url": "http://127.0.0.1:9/v1"}}}"#,
    );
    // `-h` answers before any of this is used; a run that starts and prints the
    // banner is enough to know the file was accepted rather than refused.
    let output = run_afi(&home, &["-f", "-"]);
    assert_ne!(output.status.code(), Some(2), "the file must be accepted");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!stderr.contains("config.json"), "{stderr}");
}

#[test]
fn a_named_file_that_is_not_there_exits_2() {
    let home = TempDir::new().unwrap();
    let missing = home.path().join("nope.json");
    let output = run_afi(&home, &["--config", missing.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(2), "must not start");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("nope.json"), "{stderr}");
}

#[test]
fn a_config_flag_with_no_value_exits_2() {
    let home = TempDir::new().unwrap();
    let output = run_afi(&home, &["--config"]);
    assert_eq!(output.status.code(), Some(2), "must not start");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("--config needs a value"), "{stderr}");
}

#[test]
fn the_sessions_listing_looks_where_the_file_says_runs_save() {
    // `afi sessions` answers without building a runtime, so the file has to be
    // read before it: otherwise a run saves into one directory and the listing
    // reads another, and the list looks empty for no stated reason.
    let home = TempDir::new().unwrap();
    let elsewhere = home.path().join("somewhere-else");
    fs::create_dir_all(&elsewhere).unwrap();
    let mut messages = vec![serde_json::json!({"role": "user", "content": "hello"})];
    write_session(
        &elsewhere,
        "20250101-120000-abcdef",
        &mut messages,
        Some(&serde_json::json!({"title": "a saved run"})),
    )
    .unwrap();
    user_config(
        &home,
        &format!(
            r#"{{"sessions_dir": {}}}"#,
            serde_json::to_string(&elsewhere.to_string_lossy().to_string()).unwrap()
        ),
    );
    let output = run_afi(&home, &["sessions"]);
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("a saved run"), "{stdout}");
}

#[test]
fn a_broken_file_refuses_the_sessions_listing_too() {
    let home = TempDir::new().unwrap();
    user_config(&home, r#"{"sessions_dirs": "/tmp"}"#);
    let output = run_afi(&home, &["sessions"]);
    assert_eq!(output.status.code(), Some(2), "must not list");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("sessions_dirs"), "{stderr}");
}

#[test]
fn a_named_file_replaces_the_one_that_was_found() {
    let home = TempDir::new().unwrap();
    // The file in the default location would refuse the run; the named one does
    // not, so a run that starts proves only the named one was read.
    user_config(&home, r#"{"broken": true}"#);
    let named = home.path().join("elsewhere.json");
    fs::write(&named, r#"{"max_tokens": 8000}"#).unwrap();
    let output = run_afi(&home, &["--config", named.to_str().unwrap(), "-f", "-"]);
    assert_ne!(
        output.status.code(),
        Some(2),
        "the default must not be read"
    );
}
