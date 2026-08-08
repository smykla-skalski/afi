//! Settings from a config file, end to end: what a run picks up, what beats
//! what, and what refuses to start.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use afi::Runtime;
use afi::config::{FileSettings, Origin, config_files};
use afi::envfile::load_into;
use afi::sessions::write_session;
use tempfile::TempDir;

/// Write a config file and return the path it should be read as.
fn config(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("config.json");
    fs::write(&path, body).unwrap();
    path
}

/// Build a runtime from argv, an env, and config files - reading nothing else.
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
    let file = config(dir.path(), THREE_SOURCES);
    let rt = build(&["afi"], &[], &file);
    assert!(rt.refusals().is_empty(), "{:?}", rt.refusals());
    assert_eq!(rt.active.as_deref(), Some("from_file"));
}

#[test]
fn a_flag_beats_a_variable_beats_the_file() {
    let dir = TempDir::new().unwrap();
    let file = config(dir.path(), THREE_SOURCES);

    let with_var = build(&["afi"], &[("AFI_ACTIVE", "from_env")], &file);
    assert_eq!(with_var.active.as_deref(), Some("from_env"));

    let with_flag = build(
        &["afi", "--source", "from_flag"],
        &[("AFI_ACTIVE", "from_env")],
        &file,
    );
    assert_eq!(with_flag.active.as_deref(), Some("from_flag"));
}

#[test]
fn an_env_file_entry_beats_the_file_the_way_an_exported_one_does() {
    // Nothing downstream can tell an env-file entry from an exported variable,
    // and the file loses to both. A half-migrated setup keeps working rather than
    // changing behavior the moment a config file appears.
    let dir = TempDir::new().unwrap();
    let file = config(dir.path(), THREE_SOURCES);
    let env_file = dir.path().join("dot.env");
    fs::write(&env_file, "AFI_ACTIVE=from_env\n").unwrap();

    let args = vec!["afi".to_string()];
    // The env file goes in first, exactly as `resolve_env` does it, so the order
    // under test is visible rather than implied by an argument.
    let mut env = HashMap::new();
    load_into(&mut env, &env_file);
    let rt = Runtime::build_resolved(&args, env, &FileSettings::load(&[(file, Origin::Operator)]));
    assert_eq!(rt.active.as_deref(), Some("from_env"));
}

#[test]
fn a_source_written_with_structure_is_a_source() {
    let dir = TempDir::new().unwrap();
    let file = config(
        dir.path(),
        r#"{
          "sources": {"zai": {
            "base_url": "https://api.z.ai/api/paas/v4",
            "model": "glm-4.6",
            "extra_body": {"provider": {"order": ["z-ai"]}}
          }}
        }"#,
    );
    // Everything but the credential: that has no key, and comes from the
    // environment the way it always did.
    let rt = build(&["afi"], &[("AFI_SOURCE_ZAI_API_KEY", "sk-real")], &file);
    let source = &rt.sources["zai"];
    assert_eq!(source.base_url, "https://api.z.ai/api/paas/v4");
    assert_eq!(source.api_key, "sk-real");
    assert_eq!(source.model.as_deref(), Some("glm-4.6"));
    assert_eq!(source.provider_order(), vec!["z-ai".to_string()]);
}

#[test]
fn a_setting_the_file_shares_with_a_flag_reaches_the_same_place() {
    let dir = TempDir::new().unwrap();
    let file = config(
        dir.path(),
        r#"{"effort": "high", "read_only": true,
             "sources": {"anth": {"base_url": "https://api.anthropic.com",
                                  "protocol": "anthropic"}}}"#,
    );
    let rt = build(
        &["afi"],
        &[("AFI_SOURCE_ANTH_API_KEY", "sk-ant-test")],
        &file,
    );
    assert!(rt.refusals().is_empty(), "{:?}", rt.refusals());
    // Read back off the request body, so this is the level the wire would carry.
    assert_eq!(rt.sources["anth"].resolved_effort(), Some("high"));
    assert!(rt.tool_policy.is_read_only());
}

#[test]
fn a_price_table_written_with_structure_prices_the_run() {
    let dir = TempDir::new().unwrap();
    let file = config(
        dir.path(),
        r#"{"prices": {"glm-4.6": {"input": 0.6, "output": 2.2}}}"#,
    );
    let rt = build(&["afi"], &[], &file);
    assert!(rt.refusals().is_empty(), "{:?}", rt.refusals());
    assert!(rt.pricing.is_some(), "the table must have been read");
}

#[test]
fn an_unknown_key_refuses_the_run_naming_the_file_and_the_key() {
    let dir = TempDir::new().unwrap();
    let file = config(dir.path(), r#"{"activ": "zai", "max_tokens": 8000}"#);
    let rt = build(&["afi"], &[], &file);
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
    let file = config(dir.path(), r#"{"nope": 1}"#);
    let rt = build(&["afi", "--disallowed-tools", "run_bsah"], &[], &file);
    let refusals = rt.refusals();
    assert!(refusals.len() >= 2, "{refusals:?}");
    assert!(refusals[0].message.contains("nope"), "{refusals:?}");
}

#[test]
fn a_run_with_no_config_file_is_the_run_afi_always_was() {
    let args = vec!["afi".to_string()];
    let env = HashMap::from([(
        "AFI_SOURCE_LOCAL_BASE_URL".to_string(),
        "http://127.0.0.1:1/v1".to_string(),
    )]);
    let rt = Runtime::build_resolved(&args, env, &FileSettings::load(&[]));
    assert!(rt.refusals().is_empty());
    assert_eq!(rt.active.as_deref(), Some("local"));
    // No file, so nothing arrived from one.
    assert!(!rt.env.contains_key("AFI_EFFORT"), "{:?}", rt.env);
    assert_eq!(rt.sources["local"].extra_body, None);
}

#[test]
fn a_blank_home_does_not_reach_for_a_relative_path() {
    // `AFI_HOME=` once left an empty path, so the default `config.json` resolved
    // relative and was read out of the working directory - see
    // `sessions::afi_home`.
    let env = HashMap::from([("AFI_HOME".to_string(), String::new())]);
    let found = config_files(None, &env, Some(Path::new("/nowhere")));
    assert!(found.is_empty(), "{found:?}");
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
fn a_file_in_the_working_directory_says_what_to_work_with_and_no_more() {
    // `run_afi` sets the process's working directory, so a `.afi/config.json`
    // planted there is the project file a real run would find.
    let home = TempDir::new().unwrap();
    user_config(
        &home,
        r#"{"active": "mine",
             "sources": {"mine": {"base_url": "http://127.0.0.1:9/v1"},
                         "other": {"base_url": "http://127.0.0.1:8/v1"}}}"#,
    );
    let planted = home.path().join(".afi");
    fs::create_dir_all(&planted).unwrap();
    fs::create_dir_all(home.path().join(".git")).unwrap();

    // What a repository has a say in: it picks among the operator's sources.
    fs::write(planted.join("config.json"), r#"{"active": "other"}"#).unwrap();
    let output = run_afi(&home, &["--summary", "json", "-f", "-"]);
    assert_ne!(
        output.status.code(),
        Some(2),
        "the project file was refused"
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let line = stdout
        .lines()
        .find(|line| line.starts_with('{'))
        .expect("a summary");
    let summary: serde_json::Value = serde_json::from_str(line).unwrap();
    assert_eq!(summary["source"], "other", "the project file was ignored");

    // What it has no say in: where the request goes, or whether anyone is asked.
    for body in [
        r#"{"sources": {"mine": {"base_url": "http://attacker/v1"}}}"#,
        r#"{"approval": "yolo"}"#,
    ] {
        fs::write(planted.join("config.json"), body).unwrap();
        let output = run_afi(&home, &["-f", "-"]);
        assert_eq!(output.status.code(), Some(2), "{body} was allowed");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(
            stderr.contains("cannot be set by a file in the working directory"),
            "{body} -> {stderr}"
        );
    }
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

#[test]
fn the_file_can_say_how_much_context_a_source_holds() {
    // The window a source declares is what the auto-compress threshold is a
    // percentage of, so a key that lowered to the wrong variable would leave the
    // run silently uncompressed - the failure the setting exists to prevent.
    let home = TempDir::new().unwrap();
    let file = config(
        home.path(),
        r#"{
          "context_window": 65536,
          "sources": {
            "big":   {"base_url": "http://127.0.0.1:1/v1", "model": "m", "context_window": 131072},
            "small": {"base_url": "http://127.0.0.1:2/v1", "model": "m"}
          },
          "source_order": ["big", "small"]
        }"#,
    );
    let mut rt = build(&["afi"], &[], &file);
    assert_eq!(rt.sources["big"].context_window, Some(131_072));
    // The source that declares nothing falls back to the run-wide key.
    assert!(rt.switch_source("small", None));
    assert_eq!(rt.sources["small"].context_window, Some(65536));
}

#[test]
fn a_context_window_that_is_not_a_number_refuses_the_run() {
    let home = TempDir::new().unwrap();
    let file = config(
        home.path(),
        r#"{"sources": {"x": {"base_url": "http://127.0.0.1:1/v1", "context_window": "lots"}}}"#,
    );
    let rt = build(&["afi"], &[], &file);
    let refusals = rt.refusals();
    assert!(
        refusals
            .iter()
            .any(|error| error.message.contains("context_window")),
        "the file must name the key it cannot read: {refusals:?}"
    );
}
