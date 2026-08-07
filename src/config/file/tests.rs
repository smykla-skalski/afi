//! Finding the files, and what happens when two of them say something.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::{ConfigFiles, FileSettings};

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

/// Load `files` and return what they leave in an env holding `start`.
fn applied(files: &ConfigFiles, start: &[(&str, &str)]) -> HashMap<String, String> {
    let mut out = env(start);
    FileSettings::load(files).apply_to(&mut out);
    out
}

#[test]
fn a_setting_written_in_the_file_takes_effect() {
    let dir = TempDir::new().unwrap();
    let path = write(dir.path(), "config.json", r#"{"active": "zai"}"#);
    let files = ConfigFiles { paths: vec![path] };
    assert_eq!(applied(&files, &[]).get("AFI_ACTIVE").unwrap(), "zai");
}

#[test]
fn a_variable_beats_the_file() {
    let dir = TempDir::new().unwrap();
    let path = write(dir.path(), "config.json", r#"{"active": "zai"}"#);
    let files = ConfigFiles { paths: vec![path] };
    let out = applied(&files, &[("AFI_ACTIVE", "anthropic")]);
    assert_eq!(out.get("AFI_ACTIVE").unwrap(), "anthropic");
}

#[test]
fn a_variable_set_to_nothing_is_a_gap_the_file_fills() {
    // `export AFI_ALLOWED_TOOLS="$UNSET"` is how a blank arrives. The policy reads
    // a blank list as every tool, so letting it shadow the file would widen the
    // run past both what the file said and what the variable said.
    let dir = TempDir::new().unwrap();
    let path = write(
        dir.path(),
        "config.json",
        r#"{"allowed_tools": ["read_file"], "effort": "high"}"#,
    );
    let files = ConfigFiles { paths: vec![path] };
    let out = applied(&files, &[("AFI_ALLOWED_TOOLS", ""), ("AFI_EFFORT", "  ")]);
    assert_eq!(out.get("AFI_ALLOWED_TOOLS").unwrap(), "read_file");
    assert_eq!(out.get("AFI_EFFORT").unwrap(), "high");
}

#[test]
fn a_later_file_wins_key_by_key() {
    let dir = TempDir::new().unwrap();
    let first = write(
        dir.path(),
        "one/config.json",
        r#"{"effort": "high", "active": "anthropic"}"#,
    );
    let second = write(dir.path(), "two/config.json", r#"{"active": "zai"}"#);
    let files = ConfigFiles {
        paths: vec![first, second],
    };
    let out = applied(&files, &[]);
    // The second said one thing, so it changed one thing.
    assert_eq!(out.get("AFI_ACTIVE").unwrap(), "zai");
    assert_eq!(out.get("AFI_EFFORT").unwrap(), "high");
}

#[test]
fn a_file_with_anything_wrong_in_it_applies_nothing() {
    let dir = TempDir::new().unwrap();
    let path = write(
        dir.path(),
        "config.json",
        r#"{"active": "zai", "activ": "oops"}"#,
    );
    let files = ConfigFiles { paths: vec![path] };
    let settings = FileSettings::load(&files);
    assert_eq!(settings.refusals().len(), 1);
    let mut out = env(&[]);
    settings.apply_to(&mut out);
    assert!(out.is_empty(), "a refused file applied {out:?}");
}

#[test]
fn a_bad_key_in_one_file_stops_the_other_from_applying_too() {
    // The run is about to refuse to start; applying half of what was asked for
    // would leave a caller that ignores the refusal with settings nobody chose.
    let dir = TempDir::new().unwrap();
    let good = write(dir.path(), "one/config.json", r#"{"active": "zai"}"#);
    let bad = write(dir.path(), "two/config.json", r#"{"nope": 1}"#);
    let files = ConfigFiles {
        paths: vec![good, bad],
    };
    assert!(applied(&files, &[]).is_empty());
}

#[test]
fn a_named_file_that_is_not_there_refuses_the_run() {
    let dir = TempDir::new().unwrap();
    let files = ConfigFiles {
        paths: vec![dir.path().join("missing.json")],
    };
    let refusals = FileSettings::load(&files).refusals().to_vec();
    assert_eq!(refusals.len(), 1, "{refusals:?}");
    assert!(refusals[0].contains("missing.json"), "{refusals:?}");
}

#[test]
fn no_files_at_all_leaves_the_env_exactly_as_it_was() {
    let files = ConfigFiles::default();
    let settings = FileSettings::load(&files);
    assert!(settings.refusals().is_empty());
    let out = applied(&files, &[("AFI_ACTIVE", "zai")]);
    assert_eq!(out.len(), 1);
}

#[test]
fn the_user_file_is_found_under_afi_home() {
    let dir = TempDir::new().unwrap();
    let home = dir.path().join("afi-home");
    let path = write(&home, "config.json", "{}");
    let found = ConfigFiles::discover(None, &env(&[("AFI_HOME", home.to_str().unwrap())]));
    assert_eq!(found.paths, vec![path]);
}

#[test]
fn nothing_in_the_working_tree_is_read() {
    // A `.afi/config.json` beside the run is not configuration. It was, until a
    // one-key `base_url` in a clone proved it could redirect a source and carry
    // off whatever credential `$NAME` resolves out of the operator's own
    // environment.
    let dir = TempDir::new().unwrap();
    let home = dir.path().join("afi-home");
    let user = write(&home, "config.json", "{}");
    write(dir.path(), ".afi/config.json", r#"{"active": "attacker"}"#);
    fs::create_dir_all(dir.path().join(".git")).unwrap();
    let found = ConfigFiles::discover(None, &env(&[("AFI_HOME", home.to_str().unwrap())]));
    assert_eq!(found.paths, vec![user]);
}

#[test]
fn a_named_file_replaces_the_default() {
    let dir = TempDir::new().unwrap();
    let home = dir.path().join("afi-home");
    write(&home, "config.json", "{}");
    let named = write(dir.path(), "elsewhere.json", "{}");
    let found = ConfigFiles::discover(
        Some(named.to_str().unwrap()),
        &env(&[("AFI_HOME", home.to_str().unwrap())]),
    );
    assert_eq!(found.paths, vec![named]);
}

#[test]
fn the_flag_beats_the_variable_and_a_blank_names_nothing() {
    let dir = TempDir::new().unwrap();
    let home = dir.path().join("afi-home");
    let user = write(&home, "config.json", "{}");
    let flagged = ConfigFiles::discover(
        Some("/from/flag.json"),
        &env(&[("AFI_CONFIG", "/from/env.json")]),
    );
    assert_eq!(flagged.paths, vec![PathBuf::from("/from/flag.json")]);

    let from_env = ConfigFiles::discover(None, &env(&[("AFI_CONFIG", "/from/env.json")]));
    assert_eq!(from_env.paths, vec![PathBuf::from("/from/env.json")]);

    // An exported-but-unset variable names no file and leaves the default.
    let blank = ConfigFiles::discover(
        None,
        &env(&[("AFI_CONFIG", "  "), ("AFI_HOME", home.to_str().unwrap())]),
    );
    assert_eq!(blank.paths, vec![user]);
}
