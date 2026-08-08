//! The subtree walk and the file-reading step, tested without the process-wide
//! state.
//!
//! `for_path` and `arm` share one accumulator per process, so asserting on them
//! here would make the result depend on which tests ran first. The pieces they are
//! built from are pure, and the stateful behaviour - once per directory, budget,
//! outside the project - is proved against a real process in
//! `tests/instructions_nested.rs`.

use std::fs;

use tempfile::TempDir;

use super::*;
use crate::risk::resolve_action_path;

/// The unsent instruction files a call on `dir` brings in, the way `for_path` asks
/// for them: the span up to `launch`, filtered and marked against `seen`.
fn found_from(dir: &Path, launch: &Path, seen: &mut HashSet<PathBuf>) -> Vec<PathBuf> {
    files_in(chain_up(dir, launch).into_iter(), seen)
}

/// A workspace: a root with its own file, and a crate two levels down with one.
fn workspace() -> TempDir {
    let dir = TempDir::new().expect("the temp dir must open");
    fs::create_dir_all(dir.path().join("crates/api/src")).expect("the subtree must write");
    fs::write(dir.path().join("AGENTS.md"), "root rules\n").expect("the root file must write");
    fs::write(dir.path().join("crates/api/AGENTS.md"), "api rules\n")
        .expect("the crate file must write");
    dir
}

#[test]
fn a_call_into_a_subtree_finds_the_file_above_it() {
    // The model arrives at a subtree, not at a directory: one read of
    // `crates/api/src/lib.rs` has to pick up `crates/api/AGENTS.md`.
    let dir = workspace();
    let found = found_from(
        &dir.path().join("crates/api/src"),
        dir.path(),
        &mut HashSet::new(),
    );
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(found[0].ends_with("AGENTS.md"));
    assert!(
        found[1].ends_with("crates/api/AGENTS.md"),
        "the deeper file reads last: {found:?}"
    );
}

#[test]
fn the_walk_stops_at_the_launch_directory() {
    // Everything at or above it was read at startup, and climbing past it would
    // leave the walk running to the filesystem root.
    let dir = workspace();
    let found = found_from(
        &dir.path().join("crates/api"),
        &dir.path().join("crates"),
        &mut HashSet::new(),
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].ends_with("crates/api/AGENTS.md"));
}

#[test]
fn the_walk_marks_what_it_hands_back_so_a_second_call_finds_nothing() {
    // What keeps a rule from being re-appended to every read in the same directory.
    // The walk marks rather than the reader, so a file that turns out to be unreadable
    // is not retried either.
    let dir = workspace();
    let mut seen = HashSet::new();
    let first = found_from(&dir.path().join("crates/api"), dir.path(), &mut seen);
    assert_eq!(first.len(), 2, "{first:?}");
    assert!(
        found_from(&dir.path().join("crates/api"), dir.path(), &mut seen).is_empty(),
        "the same call twice must bring nothing the second time"
    );
}

#[test]
fn a_file_already_sent_is_not_found_again() {
    // The startup walk's files arrive in the set the same way, so a directory it
    // already covered is not read a second time here.
    let dir = workspace();
    let mut sent = HashSet::from([canonical(dir.path().join("crates/api/AGENTS.md"))]);
    let found = found_from(&dir.path().join("crates/api"), dir.path(), &mut sent);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].ends_with("AGENTS.md") && !found[0].ends_with("api/AGENTS.md"));
}

#[test]
fn a_directory_with_no_instruction_file_finds_nothing() {
    let dir = workspace();
    fs::remove_file(dir.path().join("AGENTS.md")).unwrap();
    fs::remove_file(dir.path().join("crates/api/AGENTS.md")).unwrap();
    assert!(
        found_from(
            &dir.path().join("crates/api/src"),
            dir.path(),
            &mut HashSet::new()
        )
        .is_empty()
    );
}

#[test]
fn a_path_that_does_not_exist_yet_resolves_to_its_parent() {
    // The `write_file` case: the model names a file it is about to create, and the
    // directory it lands in is the one whose rules apply.
    let dir = workspace();
    let target = dir.path().join("crates/api/src/new.rs");
    assert_eq!(
        dir_of(&target).expect("a parent"),
        dir.path().join("crates/api/src")
    );
    // And a directory is its own answer, which is what `list_dir` passes.
    let listed = dir.path().join("crates/api");
    assert_eq!(dir_of(&listed).expect("itself"), listed);
}

/// A state armed at `launch` with the whole budget and nothing sent.
fn armed(launch: &Path) -> State {
    State {
        launch: canonical(launch),
        sent: HashSet::new(),
        remaining: MAX_BYTES,
        loaded: Vec::new(),
    }
}

#[test]
fn reading_a_file_sends_it_and_spends_the_budget() {
    let dir = workspace();
    let path = dir.path().join("crates/api/AGENTS.md");
    let mut state = armed(dir.path());

    let block = take(&mut state, &path).expect("a file with rules in it is a block");
    assert!(block.contains("Contents of "), "{block}");
    assert!(
        block.ends_with("api rules"),
        "trimmed as it was sent: {block}"
    );
    assert_eq!(state.loaded, vec![(path.display().to_string(), 9)]);
    assert_eq!(state.remaining, MAX_BYTES - 9);
}

#[test]
fn a_file_that_says_nothing_is_skipped_in_silence() {
    // A placeholder file, the same case the startup walk leaves out. Nothing was
    // sent, so there is nothing to tell the model about.
    let dir = workspace();
    let path = dir.path().join("crates/api/AGENTS.md");
    fs::write(&path, "\u{feff}\n  \n").unwrap();
    let mut state = armed(dir.path());

    assert!(take(&mut state, &path).is_none());
    assert!(state.loaded.is_empty());
}

#[test]
fn a_file_past_the_budget_is_reported_rather_than_sent() {
    // Truncating would leave the model following half a rule set, and dropping it
    // in silence would leave a subtree whose rules never arrived looking exactly
    // like one that has none.
    let dir = workspace();
    let path = dir.path().join("crates/api/AGENTS.md");
    let mut state = armed(dir.path());
    state.remaining = 2;

    let note = take(&mut state, &path).expect("the model is told");
    assert!(note.contains("was not read"), "{note}");
    assert!(note.contains(&MAX_BYTES.to_string()), "{note}");
    assert!(state.loaded.is_empty(), "and nothing was charged for");
    assert_eq!(state.remaining, 2);
}

#[test]
fn a_file_that_cannot_be_read_is_reported_rather_than_swallowed() {
    let dir = workspace();
    let missing = dir.path().join("crates/api/gone.md");
    let mut state = armed(dir.path());

    let note = take(&mut state, &missing).expect("the model is told");
    assert!(note.contains("could not be read"), "{note}");
    assert!(state.loaded.is_empty());
}

#[test]
fn a_path_that_climbs_out_with_dot_dot_lands_where_it_really_points() {
    // The boundary check compares canonical prefixes, and `Path::starts_with` is a
    // component-wise test: `/repo/src/../../..` starts with `/repo`. So a path whose
    // target does not exist - which is what stops `canonicalize` from resolving it -
    // used to compare as inside the project while naming a directory above it, and
    // one call pulled the parent's and grandparent's `AGENTS.md` in.
    let outer = TempDir::new().unwrap();
    let repo = outer.path().join("repo");
    fs::create_dir_all(repo.join("src")).unwrap();

    let escaped = resolve_action_path("src/../../nope/x", &repo);
    assert!(
        !escaped.starts_with(canonical(&repo)),
        "resolved to {} which still compares as inside {}",
        escaped.display(),
        repo.display()
    );
    assert!(
        escaped.starts_with(canonical(outer.path()).parent().expect("a parent")),
        "and lands where it really points: {}",
        escaped.display()
    );
    // The lexical form is the one that fooled the check, so pin that it would.
    assert!(
        repo.join("src/../../nope/x").starts_with(&repo),
        "if this ever stops being true the guard above is testing nothing"
    );
}

#[test]
fn an_ordinary_subtree_path_still_lands_inside_the_launch_dir() {
    // The other half: the fix must not start rejecting the case it exists to allow.
    // Both spellings a tool call arrives in, run through the two steps `for_path`
    // uses - resolve, then canonicalize the directory - because a path naming a file
    // that does not exist yet stays lexical and only its directory can be resolved.
    let dir = workspace();
    let launch = canonical(dir.path());
    fs::write(dir.path().join("crates/api/src/lib.rs"), "pub fn go() {}\n").unwrap();

    for path in [
        "crates/api/src/lib.rs",
        "crates/api/src/not_yet.rs",
        // Enters the `..` arm and still lands inside, which is the case the escape
        // guard must not over-reject. Models emit this shape constantly.
        "crates/api/../api/src/not_yet.rs",
    ] {
        let resolved = resolve_action_path(path, dir.path());
        let target = canonical(dir_of(&resolved).expect("a directory"));
        assert!(
            is_under_path(&target, &launch),
            "{path} resolved to {} which is outside {}",
            target.display(),
            launch.display()
        );
        assert!(target.ends_with("crates/api/src"), "{}", target.display());
    }
}

#[test]
fn an_unarmed_run_reads_nothing_and_reports_nothing() {
    // A run that named its files, or asked for nothing at all. `arm` is what turns
    // this module on, and it is never called for either.
    arm(
        super::super::super::system_prompt::builtin(),
        Path::new("/"),
    );
    assert!(for_path(Path::new("/tmp")).is_none());
    assert!(loaded().is_empty());
}
