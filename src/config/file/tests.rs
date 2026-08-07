//! Finding the files, and what happens when two of them say something.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::{FileSettings, Origin, config_files};

mod coverage;
mod lowering;
mod suggesting;
mod trust;

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
fn applied(files: &[(PathBuf, Origin)], start: &[(&str, &str)]) -> HashMap<String, String> {
    let mut out = env(start);
    FileSettings::load(files).apply_to(&mut out);
    out
}

/// One operator-owned file, which is what most of these are about.
fn mine(path: PathBuf) -> Vec<(PathBuf, Origin)> {
    vec![(path, Origin::Operator)]
}

#[test]
fn a_setting_written_in_the_file_takes_effect() {
    let dir = TempDir::new().unwrap();
    let path = write(dir.path(), "config.json", r#"{"active": "zai"}"#);
    assert_eq!(applied(&mine(path), &[]).get("AFI_ACTIVE").unwrap(), "zai");
}

#[test]
fn a_variable_beats_the_file() {
    let dir = TempDir::new().unwrap();
    let path = write(dir.path(), "config.json", r#"{"active": "zai"}"#);
    let out = applied(&mine(path), &[("AFI_ACTIVE", "anthropic")]);
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
    let out = applied(&mine(path), &[("AFI_SUMMARY_FILE", "")]);
    assert_eq!(out.get("AFI_SUMMARY_FILE").unwrap(), "");
    // A variable that says nothing at all is still a gap.
    assert_eq!(out.get("AFI_ACTIVE").unwrap(), "zai");
}

#[test]
fn a_project_file_cannot_widen_what_the_operator_allowed() {
    // Replacing would let the working tree add a tool the operator did not
    // permit, or drop one they denied. These combine instead.
    let dir = TempDir::new().unwrap();
    let user = write(
        dir.path(),
        "user/config.json",
        r#"{"allowed_tools": ["read_file", "list_dir"],
             "disallowed_tools": ["run_bash"],
             "read_only": true}"#,
    );
    let project = write(
        dir.path(),
        "repo/config.json",
        r#"{"allowed_tools": ["read_file", "run_bash"],
             "disallowed_tools": [],
             "read_only": false}"#,
    );
    let out = applied(
        &[(user, Origin::Operator), (project, Origin::WorkingTree)],
        &[],
    );
    // Only what both agree on, so `run_bash` is not permitted by asking twice.
    assert_eq!(out.get("AFI_ALLOWED_TOOLS").unwrap(), "read_file");
    // The denial stands even though the project file named none.
    assert_eq!(out.get("AFI_DISALLOWED_TOOLS").unwrap(), "run_bash");
    // And the posture does not come back off.
    assert_eq!(out.get("AFI_READ_ONLY").unwrap(), "1");
}

#[test]
fn a_project_file_may_tighten_what_the_operator_allowed() {
    let dir = TempDir::new().unwrap();
    let user = write(
        dir.path(),
        "user/config.json",
        r#"{"allowed_tools": ["read_file", "list_dir", "run_bash"]}"#,
    );
    let project = write(
        dir.path(),
        "repo/config.json",
        r#"{"disallowed_tools": ["run_bash"], "read_only": true}"#,
    );
    let out = applied(
        &[(user, Origin::Operator), (project, Origin::WorkingTree)],
        &[],
    );
    assert_eq!(out.get("AFI_DISALLOWED_TOOLS").unwrap(), "run_bash");
    assert_eq!(out.get("AFI_READ_ONLY").unwrap(), "1");
}

#[test]
fn two_allow_lists_with_nothing_in_common_permit_nothing() {
    // An empty list reads as "every tool" downstream, so it cannot be the answer
    // to "these two agreed on nothing" - the run refuses over the name instead.
    let dir = TempDir::new().unwrap();
    let user = write(
        dir.path(),
        "user/config.json",
        r#"{"allowed_tools": ["read_file"]}"#,
    );
    let project = write(
        dir.path(),
        "repo/config.json",
        r#"{"allowed_tools": ["run_bash"]}"#,
    );
    let out = applied(
        &[(user, Origin::Operator), (project, Origin::WorkingTree)],
        &[],
    );
    let allowed = out.get("AFI_ALLOWED_TOOLS").unwrap();
    assert!(
        !allowed.trim().is_empty(),
        "an empty list would grant every tool"
    );
    assert!(!allowed.contains("read_file"), "{allowed}");
    assert!(!allowed.contains("run_bash"), "{allowed}");
}

#[test]
fn a_file_with_anything_wrong_in_it_applies_nothing() {
    let dir = TempDir::new().unwrap();
    let path = write(
        dir.path(),
        "config.json",
        r#"{"active": "zai", "activ": "oops"}"#,
    );
    let settings = FileSettings::load(&mine(path));
    assert_eq!(settings.refusals().len(), 1);
    let mut out = env(&[]);
    settings.apply_to(&mut out);
    assert!(out.is_empty(), "a refused file applied {out:?}");
}

#[test]
fn a_named_file_that_is_not_there_refuses_the_run() {
    let dir = TempDir::new().unwrap();
    let missing = dir.path().join("missing.json");
    let refusals = FileSettings::load(&mine(missing)).refusals().to_vec();
    assert_eq!(refusals.len(), 1, "{refusals:?}");
    assert!(refusals[0].contains("missing.json"), "{refusals:?}");
}

#[test]
fn no_file_at_all_leaves_the_env_exactly_as_it_was() {
    let settings = FileSettings::load(&[]);
    assert!(settings.refusals().is_empty());
    let out = applied(&[], &[("AFI_ACTIVE", "zai")]);
    assert_eq!(out.len(), 1);
}

#[test]
fn the_user_file_is_found_under_afi_home() {
    let dir = TempDir::new().unwrap();
    let home = dir.path().join("afi-home");
    let path = write(&home, "config.json", "{}");
    let found = config_files(
        None,
        &env(&[("AFI_HOME", home.to_str().unwrap())]),
        Some(Path::new("/nowhere")),
    );
    assert_eq!(found, vec![(path, Origin::Operator)]);
}

#[test]
fn a_blank_home_does_not_reach_for_a_relative_path() {
    // `AFI_HOME=` once left an empty path, so `config.json` resolved relative and
    // the default location became the working directory - the one place this layer
    // promises never to look. See `sessions::afi_home`.
    let found = config_files(None, &env(&[("AFI_HOME", "")]), Some(Path::new("/nowhere")));
    assert!(
        found.is_empty(),
        "a blank home must not name a relative path"
    );
}

#[test]
fn a_named_file_replaces_the_default() {
    let dir = TempDir::new().unwrap();
    let home = dir.path().join("afi-home");
    write(&home, "config.json", "{}");
    let named = write(dir.path(), "elsewhere.json", "{}");
    let found = config_files(
        Some(named.to_str().unwrap()),
        &env(&[("AFI_HOME", home.to_str().unwrap())]),
        Some(Path::new("/nowhere")),
    );
    assert_eq!(found, vec![(named, Origin::Operator)]);
}

#[test]
fn the_flag_beats_the_variable_and_a_blank_names_nothing() {
    let dir = TempDir::new().unwrap();
    let home = dir.path().join("afi-home");
    let user = write(&home, "config.json", "{}");
    let flagged = config_files(
        Some("/from/flag.json"),
        &env(&[("AFI_CONFIG", "/from/env.json")]),
        Some(Path::new("/nowhere")),
    );
    assert_eq!(
        flagged,
        vec![(PathBuf::from("/from/flag.json"), Origin::Operator)]
    );

    let from_env = config_files(
        None,
        &env(&[("AFI_CONFIG", "/from/env.json")]),
        Some(Path::new("/nowhere")),
    );
    assert_eq!(
        from_env,
        vec![(PathBuf::from("/from/env.json"), Origin::Operator)]
    );

    // An exported-but-unset variable names no file and leaves the default.
    let blank = config_files(
        None,
        &env(&[("AFI_CONFIG", "  "), ("AFI_HOME", home.to_str().unwrap())]),
        Some(Path::new("/nowhere")),
    );
    assert_eq!(blank, vec![(user, Origin::Operator)]);
}
