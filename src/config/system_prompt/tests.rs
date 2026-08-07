use std::fs;

use tempfile::TempDir;

use super::*;

/// A prompt file holding `body`, and the directory keeping it alive.
fn prompt_file(body: &str) -> (TempDir, String) {
    let dir = TempDir::new().expect("the temp dir must open");
    let path = dir.path().join("review.md");
    fs::write(&path, body).expect("the prompt must write");
    let path = path.to_string_lossy().into_owned();
    (dir, path)
}

#[test]
fn nothing_configured_sends_the_built_in_prompt_unchanged() {
    // The requirement the rest of this module is measured against: a run that
    // asks for nothing has to put the same bytes on the wire it always has, or
    // every existing Anthropic cache entry misses on the first turn.
    let resolved = resolve(None, None).expect("the built-in prompt always resolves");
    assert_eq!(resolved.text(), prompt::system());
    assert_eq!(resolved.mode(), "builtin");
    assert_eq!(resolved.file(), None);
}

#[test]
fn a_blank_variable_names_no_file() {
    // What an exported-but-unset shell variable looks like. The flags refuse it;
    // the variables cannot, or `AFI_SYSTEM_PROMPT_FILE=` in a workflow's env
    // block would refuse every job that did not set it.
    let resolved = resolve(Some("  "), Some("")).expect("a blank pair configures nothing");
    assert_eq!(resolved.text(), prompt::system());
    assert_eq!(resolved.mode(), "builtin");
}

#[test]
fn replace_is_the_default_and_keeps_the_wire_contract() {
    let (_dir, path) = prompt_file("Review the diff. Do not write files.\n");
    let resolved = resolve(Some(&path), None).expect("the file resolves");

    assert_eq!(resolved.mode(), "replace");
    assert_eq!(resolved.file(), Some(path.as_str()));
    assert!(resolved.text().contains("Review the diff."));
    assert!(
        resolved.text().starts_with(&prompt::tool_protocol()),
        "the contract leads, so what the operator wrote reads last"
    );
    assert!(
        !resolved.text().contains("Operating principles"),
        "the shell guidance is what replacing is for"
    );
    assert!(
        !resolved.text().contains("You are a terminal coding agent"),
        "a replaced prompt says who the agent is itself"
    );
}

#[test]
fn append_keeps_the_whole_built_in_prompt() {
    let (_dir, path) = prompt_file("Also: never touch the lockfile.\n");
    let resolved = resolve(Some(&path), Some("append")).expect("the file resolves");

    assert_eq!(resolved.mode(), "append");
    assert_eq!(
        resolved.text(),
        format!("{}\n\nAlso: never touch the lockfile.", prompt::system()),
        "the built-in prompt whole, then the supplied text"
    );
}

#[test]
fn a_mode_is_case_and_space_insensitive() {
    let (_dir, path) = prompt_file("instructions");
    let resolved = resolve(Some(&path), Some(" Append ")).expect("the file resolves");
    assert_eq!(resolved.mode(), "append");
}

#[test]
fn an_unknown_mode_refuses_the_run() {
    // The typo this exists for: `repalce` taking the default would send a
    // complete, plausible run that was told something other than what was asked.
    let (_dir, path) = prompt_file("instructions");
    let error = resolve(Some(&path), Some("repalce")).expect_err("an unknown mode must refuse");
    assert!(error.contains("repalce"), "the refusal quotes the value");
    assert!(
        error.contains("replace") && error.contains("append"),
        "{error}"
    );
}

#[test]
fn an_unknown_mode_refuses_even_with_no_file() {
    // Nothing would go wrong on this run, since the mode applies to nothing. It
    // is still the moment the mistake is visible, and the next run is the one
    // that supplies a file.
    resolve(None, Some("REPLACE ALL")).expect_err("an unknown mode must refuse");
}

#[test]
fn a_missing_file_refuses_the_run_naming_the_path() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("absent.md");
    let path = path.to_string_lossy().into_owned();

    let error = resolve(Some(&path), None).expect_err("a missing file must refuse");
    assert!(
        error.contains(&path),
        "the refusal must name the path it could not read: {error}"
    );
}

#[test]
fn a_directory_refuses_the_run_naming_the_path() {
    // The shape of `--system-prompt-file "$PROMPT_DIR"`, which reads as a path
    // that exists right up to the point something tries to read it.
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_string_lossy().into_owned();

    let error = resolve(Some(&path), None).expect_err("a directory must refuse");
    assert!(error.contains(&path), "{error}");
}

#[test]
fn an_empty_file_refuses_the_run_rather_than_falling_back() {
    // What a truncated write and an unexpanded template both leave behind. The
    // built-in prompt is the one answer that must not happen here.
    for body in ["", "\n", "   \n\t\n"] {
        let (_dir, path) = prompt_file(body);
        let error = resolve(Some(&path), None)
            .expect_err(&format!("an empty prompt must refuse, gave {body:?}"));
        assert!(error.contains(&path), "{error}");
        assert!(error.contains("empty"), "{error}");
    }
}

#[test]
fn surrounding_blank_lines_are_trimmed_off_the_supplied_text() {
    // The seam between afi's part and the operator's is one blank line in both
    // modes. A file that ends with a newline, which every editor writes, must
    // not turn that into two.
    let (_dir, path) = prompt_file("\n\nReview the diff.\n\n\n");
    let resolved = resolve(Some(&path), Some("append")).expect("the file resolves");
    assert!(resolved.text().ends_with("\n\nReview the diff."));
    assert!(!resolved.text().contains("\n\n\n"));
}
