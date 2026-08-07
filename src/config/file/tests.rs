//! Finding the files, and what happens when two of them say something.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::{FileSettings, config_path};

mod coverage;
mod lowering;
mod suggesting;

/// Write `body` to `dir/relative`, creating the parents.
fn write(dir: &Path, relative: &str, body: &str) -> PathBuf {
    let path = dir.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, body).unwrap();
    path
}

/// An env map from pairs, so a test says only what it sets.
fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

/// Load `file` and return what it leaves in an env holding `start`.
fn applied(file: Option<&Path>, start: &[(&str, &str)]) -> HashMap<String, String> {
    let mut out = env(start);
    FileSettings::load(file).apply_to(&mut out);
    out
}

#[test]
fn a_setting_written_in_the_file_takes_effect() {
    let dir = TempDir::new().unwrap();
    let path = write(dir.path(), "config.json", r#"{"active": "zai"}"#);
    let files = vec![path];
    assert_eq!(
        applied(files.first().map(PathBuf::as_path), &[])
            .get("AFI_ACTIVE")
            .unwrap(),
        "zai"
    );
}

#[test]
fn a_variable_beats_the_file() {
    let dir = TempDir::new().unwrap();
    let path = write(dir.path(), "config.json", r#"{"active": "zai"}"#);
    let files = vec![path];
    let out = applied(
        files.first().map(PathBuf::as_path),
        &[("AFI_ACTIVE", "anthropic")],
    );
    assert_eq!(out.get("AFI_ACTIVE").unwrap(), "anthropic");
}

#[test]
fn a_variable_set_to_nothing_still_beats_the_file() {
    // Blank is a value for several of these, and the value means "off":
    // `AFI_SUMMARY_FILE=` names no file, `AFI_SYSTEM_PROMPT_FILE=` sends afi's own
    // prompt. Filling those from the file would make the run write a file that was
    // suppressed, or send instructions that were switched off.
    let dir = TempDir::new().unwrap();
    let path = write(
        dir.path(),
        "config.json",
        r#"{"summary_file": "/tmp/from-file.json", "active": "zai"}"#,
    );
    let out = applied(Some(&path), &[("AFI_SUMMARY_FILE", "")]);
    assert_eq!(out.get("AFI_SUMMARY_FILE").unwrap(), "");
    // A variable that says nothing at all is still a gap.
    assert_eq!(out.get("AFI_ACTIVE").unwrap(), "zai");
}

#[test]
fn a_file_with_anything_wrong_in_it_applies_nothing() {
    let dir = TempDir::new().unwrap();
    let path = write(
        dir.path(),
        "config.json",
        r#"{"active": "zai", "activ": "oops"}"#,
    );
    let files = vec![path];
    let settings = FileSettings::load(files.first().map(PathBuf::as_path));
    assert_eq!(settings.refusals().len(), 1);
    let mut out = env(&[]);
    settings.apply_to(&mut out);
    assert!(out.is_empty(), "a refused file applied {out:?}");
}

#[test]
fn a_named_file_that_is_not_there_refuses_the_run() {
    let dir = TempDir::new().unwrap();
    let files = vec![dir.path().join("missing.json")];
    let refusals = FileSettings::load(files.first().map(PathBuf::as_path))
        .refusals()
        .to_vec();
    assert_eq!(refusals.len(), 1, "{refusals:?}");
    assert!(refusals[0].contains("missing.json"), "{refusals:?}");
}

#[test]
fn no_file_at_all_leaves_the_env_exactly_as_it_was() {
    let settings = FileSettings::load(None);
    assert!(settings.refusals().is_empty());
    let out = applied(None, &[("AFI_ACTIVE", "zai")]);
    assert_eq!(out.len(), 1);
}

#[test]
fn the_user_file_is_found_under_afi_home() {
    let dir = TempDir::new().unwrap();
    let home = dir.path().join("afi-home");
    let path = write(&home, "config.json", "{}");
    let found = config_path(None, &env(&[("AFI_HOME", home.to_str().unwrap())]));
    assert_eq!(found, Some(path));
}

#[test]
fn a_blank_home_does_not_reach_for_a_relative_path() {
    // `AFI_HOME=` once left an empty path, so `config.json` resolved relative and
    // the default location became the working directory - the one place this layer
    // promises never to look. See `sessions::afi_home`.
    let found = config_path(None, &env(&[("AFI_HOME", "")]));
    assert_eq!(found, None, "a blank home must not name a relative path");
}

#[test]
fn a_named_file_replaces_the_default() {
    let dir = TempDir::new().unwrap();
    let home = dir.path().join("afi-home");
    write(&home, "config.json", "{}");
    let named = write(dir.path(), "elsewhere.json", "{}");
    let found = config_path(
        Some(named.to_str().unwrap()),
        &env(&[("AFI_HOME", home.to_str().unwrap())]),
    );
    assert_eq!(found, Some(named));
}

#[test]
fn the_flag_beats_the_variable_and_a_blank_names_nothing() {
    let dir = TempDir::new().unwrap();
    let home = dir.path().join("afi-home");
    let user = write(&home, "config.json", "{}");
    let flagged = config_path(
        Some("/from/flag.json"),
        &env(&[("AFI_CONFIG", "/from/env.json")]),
    );
    assert_eq!(flagged, Some(PathBuf::from("/from/flag.json")));

    let from_env = config_path(None, &env(&[("AFI_CONFIG", "/from/env.json")]));
    assert_eq!(from_env, Some(PathBuf::from("/from/env.json")));

    // An exported-but-unset variable names no file and leaves the default.
    let blank = config_path(
        None,
        &env(&[("AFI_CONFIG", "  "), ("AFI_HOME", home.to_str().unwrap())]),
    );
    assert_eq!(blank, Some(user));
}
