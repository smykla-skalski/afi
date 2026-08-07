//! What the path half of the summary promises: that a caller who asked for a
//! file gets one whole object there, and that a path it cannot reach is refused
//! before the run is paid for rather than after.

use super::*;
use crate::summary::SummaryFormat;
use crate::summary::tests::{summary, totals};
use serde_json::json;
use tempfile::TempDir;

#[test]
fn a_path_is_taken_only_when_one_was_actually_given() {
    assert_eq!(
        summary_path(Some("  /tmp/run.json  ")),
        Some(PathBuf::from("/tmp/run.json")),
        "surrounding space in an env file must not become part of the path"
    );
    for absent in [None, Some(""), Some("   ")] {
        assert_eq!(
            summary_path(absent),
            None,
            "{absent:?} must not name a file"
        );
    }
}

#[test]
fn naming_a_file_does_not_claim_stdout() {
    // The two are asked for separately. Implying `--summary json` would divert
    // human output to stderr, taking away the readable rendering that wanting a
    // file instead of a pipe was about keeping.
    assert!(!SummaryFormat::from_value(None).is_json());
    assert!(summary_path(Some("/tmp/run.json")).is_some());
}

#[test]
fn the_written_file_is_the_object_and_a_newline() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("run.json");
    let summary = summary(true, "done", totals(2)).to_json();

    write_file(&path, &summary).expect("a fresh path must be writable");

    let body = fs::read_to_string(&path).unwrap();
    assert!(body.ends_with('\n'), "no trailing newline: {body:?}");
    let parsed: Value = serde_json::from_str(&body).expect("the file must parse whole");
    assert_eq!(parsed, summary);
}

#[test]
fn a_rerun_replaces_the_previous_summary_rather_than_appending() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("run.json");

    write_file(&path, &summary(true, "first", totals(1)).to_json()).unwrap();
    write_file(&path, &summary(true, "second", totals(1)).to_json()).unwrap();

    let parsed: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(parsed["answer"], "second");
}

#[test]
fn no_temp_copy_is_left_beside_the_summary() {
    // A workflow collecting `*.json` from the directory should find one file.
    // The temp file's placement and its refusal to follow a planted name are
    // `crate::atomic`'s to prove.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("run.json");

    write_file(&path, &summary(true, "done", totals(1)).to_json()).unwrap();

    let left: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(left.len(), 1, "expected only the summary, got {left:?}");
}

#[test]
fn a_missing_directory_is_refused_before_the_run_and_names_the_path() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("no-such-dir/run.json");

    let error = writable(&path).expect_err("a missing directory must be refused");

    assert!(error.contains("no-such-dir/run.json"), "{error}");
    // And the write agrees, so the check is not a second opinion about the path.
    assert!(write_file(&path, &json!({})).is_err());
}

#[test]
fn a_directory_in_place_of_the_file_is_refused_before_the_run() {
    // The probe writes a sibling, which succeeds beside a directory - only the
    // rename at the end of the run would fail, long after the tokens are spent.
    let dir = TempDir::new().unwrap();
    let error = writable(dir.path()).expect_err("a directory must be refused");
    assert!(error.contains("is a directory"), "{error}");
}

#[test]
fn a_trailing_slash_is_refused_before_the_run_even_where_nothing_exists() {
    // `--summary-file "$OUTDIR/$NAME"` with `NAME` unset. `file_name` strips the
    // separator, so the probe writes an ordinary sibling of the parent and
    // passes; the rename then fails at the end of a run already paid for.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("missing/");

    let error = writable(&path).expect_err("a trailing separator must be refused");

    assert!(error.contains("names a directory"), "{error}");
    // And the write agrees, so the check is not a second opinion about the path.
    assert!(write_file(&path, &json!({})).is_err());
}

#[test]
fn a_writable_path_passes_the_check_without_creating_it() {
    // A summary from a previous run has to stay readable until this one has a
    // whole object to put in its place, so the check must not truncate anything.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("run.json");
    fs::write(&path, "previous\n").unwrap();

    writable(&path).expect("an existing file must be writable");

    assert_eq!(fs::read_to_string(&path).unwrap(), "previous\n");
}
