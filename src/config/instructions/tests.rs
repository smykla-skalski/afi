use std::fs;

use tempfile::TempDir;

use super::*;

/// An `AFI_HOME` with nothing in it, and the env naming it.
///
/// Every call passes one, even the tests with no operator file to find. Without it
/// `afi_home` falls back to `~/.afi`, and the suite would read whatever standing
/// instructions the developer running it keeps there.
fn no_home() -> (TempDir, HashMap<String, String>) {
    let dir = TempDir::new().expect("the temp dir must open");
    let env = env_at(dir.path());
    (dir, env)
}

/// An env whose `AFI_HOME` is `home`.
fn env_at(home: &Path) -> HashMap<String, String> {
    HashMap::from([("AFI_HOME".to_string(), home.to_string_lossy().into_owned())])
}

/// A tree that looks like a checkout: a `.git` directory to bound the walk, and
/// whatever files the test names, written relative to the root.
fn tree(files: &[(&str, &str)]) -> TempDir {
    let dir = TempDir::new().expect("the temp dir must open");
    fs::create_dir(dir.path().join(".git")).expect("the marker must write");
    write_all(dir.path(), files);
    dir
}

/// Write `files` under `at`, creating the directories they name.
fn write_all(at: &Path, files: &[(&str, &str)]) {
    for (path, body) in files {
        let to = at.join(path);
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent).expect("the parent must write");
        }
        fs::write(&to, body).expect("the file must write");
    }
}

/// The instructions a walk of `dir` loads, with no operator file in play, or the
/// reason it refused.
fn walk(dir: &TempDir, from: &str) -> Result<Instructions, String> {
    let (_home, env) = no_home();
    resolve(Some(DISCOVER), Some(&dir.path().join(from)), &env)
}

/// The loaded paths, shortened to their file names so a test can assert on them
/// without the temp directory in front.
fn names(loaded: &Instructions) -> Vec<String> {
    loaded
        .loaded
        .iter()
        .map(|file| {
            Path::new(&file.path)
                .file_name()
                .expect("a file name")
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

/// The loaded paths whole, in order.
fn paths(loaded: &Instructions) -> Vec<String> {
    loaded.files().into_iter().map(|(path, _)| path).collect()
}

#[test]
fn nothing_configured_loads_nothing() {
    // The requirement the rest of this module is measured against. A repository's
    // instruction files are written by whoever wrote the repository, so a run that
    // did not ask for them must not have read them - and a run that configures
    // nothing has to put the same bytes on the wire it always has.
    let (_home, env) = no_home();
    for setting in [None, Some(""), Some("  ")] {
        let loaded = resolve(setting, None, &env).expect("nothing configured always resolves");
        assert!(loaded.block().is_none(), "{setting:?} loaded something");
        assert_eq!(paths(&loaded), Vec::<String>::new());
    }
}

#[test]
fn none_turns_off_what_an_operator_file_turned_on() {
    // The value exists for the run under a `config.json` or a workflow env block
    // that set `project`, where leaving the variable out is not available.
    let (_home, env) = no_home();
    let dir = tree(&[("AGENTS.md", "Use mise, never raw cargo.\n")]);
    for off in ["none", " NONE "] {
        let loaded = resolve(Some(off), Some(dir.path()), &env).expect("none must always resolve");
        assert!(loaded.block().is_none(), "{off:?} loaded something");
    }
}

#[test]
fn the_walk_reads_both_names_deepest_last() {
    // Deepest last is what makes a subtree file win, since that is the order the
    // blocks are sent in and both file names document themselves that way.
    let dir = tree(&[
        ("AGENTS.md", "root agents\n"),
        ("CLAUDE.md", "root claude\n"),
        ("crates/api/AGENTS.md", "api agents\n"),
    ]);
    let loaded = walk(&dir, "crates/api").expect("the walk resolves");
    assert_eq!(
        names(&loaded),
        ["AGENTS.md", "CLAUDE.md", "AGENTS.md"],
        "root pair first, then the subtree's"
    );
    let block = loaded.block().expect("something was loaded");
    let root = block
        .find("root agents")
        .expect("the root file is in there");
    let deep = block.find("api agents").expect("the deep file is in there");
    assert!(root < deep, "the deeper file's text has to come last");
}

#[test]
fn the_operator_file_reads_first_and_a_project_answers_after_it() {
    // Broadest scope first: the operator's own file applies to every project, so a
    // repository's answer has to read after it and win where the two disagree.
    let home = TempDir::new().unwrap();
    fs::write(home.path().join("AGENTS.md"), "always answer in English\n").unwrap();
    let dir = tree(&[("AGENTS.md", "in this repo, use mise\n")]);

    let loaded =
        resolve(Some(DISCOVER), Some(dir.path()), &env_at(home.path())).expect("the walk resolves");
    let block = loaded.block().expect("something was loaded");
    let mine = block.find("always answer").expect("the operator's file");
    let theirs = block.find("in this repo").expect("the project's file");
    assert!(mine < theirs, "the operator's file leads: {block}");
}

#[test]
fn the_operator_file_is_read_even_with_no_project_file_at_all() {
    let home = TempDir::new().unwrap();
    fs::write(home.path().join("CLAUDE.md"), "always answer in English\n").unwrap();
    let dir = tree(&[("src/main.rs", "fn main() {}\n")]);

    let loaded =
        resolve(Some(DISCOVER), Some(dir.path()), &env_at(home.path())).expect("the walk resolves");
    assert_eq!(names(&loaded), ["CLAUDE.md"]);
}

#[test]
fn a_file_reached_twice_is_sent_once() {
    // `AFI_HOME` at `~/.afi` with the working directory at `~/.afi` finds the same
    // file as both, and sending it twice would pay for it twice on every request.
    let home = tree(&[("AGENTS.md", "the one file\n")]);
    let loaded = resolve(Some(DISCOVER), Some(home.path()), &env_at(home.path()))
        .expect("the walk resolves");
    assert_eq!(names(&loaded), ["AGENTS.md"]);
}

#[test]
fn the_walk_stops_at_the_project_boundary() {
    // Without the boundary the walk reaches `$HOME` and reads whatever standing
    // instructions live up there as though this project had written them.
    let (_home, env) = no_home();
    let outer = TempDir::new().unwrap();
    fs::write(outer.path().join("AGENTS.md"), "not this project\n").unwrap();
    let inner = outer.path().join("checkout");
    fs::create_dir_all(inner.join(".git")).unwrap();
    fs::write(inner.join("AGENTS.md"), "this project\n").unwrap();

    let loaded = resolve(Some(DISCOVER), Some(&inner), &env).expect("the walk resolves");
    let block = loaded.block().expect("something was loaded");
    assert!(block.contains("this project"));
    assert!(
        !block.contains("not this project"),
        "the walk climbed past the repository: {block}"
    );
}

#[test]
fn outside_a_repository_only_the_working_directory_is_read() {
    // Same reason as the boundary above: with nothing to stop at, the only safe
    // walk is no walk.
    let (_home, env) = no_home();
    let outer = TempDir::new().unwrap();
    fs::write(outer.path().join("AGENTS.md"), "one up\n").unwrap();
    let here = outer.path().join("work");
    fs::create_dir_all(&here).unwrap();
    fs::write(here.join("AGENTS.md"), "right here\n").unwrap();

    let loaded = resolve(Some(DISCOVER), Some(&here), &env).expect("the walk resolves");
    assert_eq!(names(&loaded), ["AGENTS.md"]);
    assert!(loaded.block().expect("loaded").contains("right here"));
}

#[test]
fn a_tree_with_no_instruction_files_loads_nothing() {
    let dir = tree(&[("src/main.rs", "fn main() {}\n")]);
    let loaded = walk(&dir, "src").expect("an ordinary checkout must not refuse");
    assert!(loaded.block().is_none());
}

#[test]
fn a_discovered_file_holding_nothing_is_left_out_rather_than_refused() {
    // A placeholder `AGENTS.md` is a fact about the repository, not a mistake in
    // the invocation, and it must not stop a run. Leaving it out of the loaded
    // paths is the report - the summary then names what was really sent.
    let dir = tree(&[
        ("AGENTS.md", "\u{feff}\n  \n"),
        ("CLAUDE.md", "real instructions\n"),
    ]);
    let loaded = walk(&dir, ".").expect("an empty file must not refuse the walk");
    assert_eq!(names(&loaded), ["CLAUDE.md"]);
}

#[test]
fn a_named_file_holding_nothing_refuses_the_run() {
    // The other half of the pair: a path the run named is an instruction to send
    // that file, and empty is what a truncated write and an unexpanded template
    // both leave behind.
    let (_home, env) = no_home();
    let dir = tree(&[("ci/rules.md", "\n\n")]);
    let path = dir.path().join("ci/rules.md").display().to_string();
    let error = resolve(Some(&path), None, &env).expect_err("an empty named file must refuse");
    assert!(error.contains(&path), "{error}");
    assert!(error.contains("empty"), "{error}");
}

#[test]
fn named_paths_are_loaded_in_the_order_they_were_written() {
    // What a CI job uses: the rules come from a path the reviewed branch cannot
    // reach, in an order the job decides. Naming files is not a walk, so the
    // operator's own file is not read either.
    let home = tree(&[("AGENTS.md", "not this one\n")]);
    let dir = tree(&[
        ("ci/review.md", "review rules\n"),
        ("ci/format.md", "format policy\n"),
    ]);
    let first = dir.path().join("ci/review.md").display().to_string();
    let second = dir.path().join("ci/format.md").display().to_string();
    let loaded = resolve(
        Some(&format!("{first}, {second}")),
        None,
        &env_at(home.path()),
    )
    .expect("both named files resolve");

    assert_eq!(paths(&loaded), [first, second]);
}

#[test]
fn a_named_path_that_is_missing_refuses_the_run() {
    let (_home, env) = no_home();
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("absent.md").display().to_string();
    let error = resolve(Some(&path), None, &env).expect_err("a missing file must refuse");
    assert!(error.contains(&path), "the refusal names the path: {error}");
}

#[test]
fn a_named_directory_refuses_the_run() {
    // The shape of `--instructions "$RULES_DIR"`, which reads as a path that
    // exists right up to the point something tries to read it.
    let (_home, env) = no_home();
    let dir = TempDir::new().unwrap();
    let path = dir.path().display().to_string();
    let error = resolve(Some(&path), None, &env).expect_err("a directory must refuse");
    assert!(error.contains(&path), "{error}");
}

#[test]
fn a_value_naming_no_file_refuses_rather_than_loading_nothing() {
    // `--instructions ,,` is a mistake, and loading nothing would be a run that
    // reports following the rules it was pointed at while following none.
    let (_home, env) = no_home();
    let error =
        resolve(Some(" , ,, "), None, &env).expect_err("a value naming nothing must refuse");
    assert!(error.contains(DISCOVER) && error.contains(OFF), "{error}");
}

#[test]
fn a_path_with_a_space_in_it_is_one_path() {
    // Every other list afi reads separates on spaces too. These are paths, so a
    // directory with a space in it would become two files that do not exist.
    let (_home, env) = no_home();
    let dir = tree(&[("my rules/AGENTS.md", "spaced\n")]);
    let path = dir.path().join("my rules/AGENTS.md").display().to_string();
    let loaded = resolve(Some(&path), None, &env).expect("one path with a space resolves");
    assert_eq!(paths(&loaded), [path]);
}

#[test]
fn too_much_instruction_text_refuses_the_run_naming_the_total() {
    // These bytes ride in front of every request and the whole history is resent
    // each turn, so the cap is about what the run pays repeatedly. Refusing beats
    // truncating: half of a rule set is instructions nobody wrote.
    let big = "x".repeat(MAX_BYTES / 2 + 1);
    let dir = tree(&[("AGENTS.md", big.as_str()), ("CLAUDE.md", big.as_str())]);
    let error = walk(&dir, ".").expect_err("over the cap must refuse");

    assert!(error.contains(&MAX_BYTES.to_string()), "{error}");
    assert!(
        error.contains("AGENTS.md") && error.contains("CLAUDE.md"),
        "{error}"
    );
    assert!(error.contains("--instructions"), "the way out: {error}");
}

#[test]
fn text_just_under_the_cap_still_loads() {
    let dir = tree(&[("AGENTS.md", "y".repeat(MAX_BYTES).as_str())]);
    let loaded = walk(&dir, ".").expect("the cap is a ceiling, not a target");
    assert_eq!(names(&loaded), ["AGENTS.md"]);
}

#[test]
fn what_was_loaded_is_reported_with_the_bytes_that_were_sent() {
    // The `/instructions` listing reads this. The size is what went in front of the
    // model, so an edit made mid-session shows up as the difference between this
    // number and the file on disk.
    let dir = tree(&[("AGENTS.md", "  use mise\n\n")]);
    let files = walk(&dir, ".").expect("the walk resolves").files();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].1, "use mise".len(), "trimmed, as it was sent");
}

#[test]
fn a_block_is_recognized_again_from_the_text_it_became() {
    // What a resumed run has to work with: the wire text is the only record that a
    // replayed tool result already carries a project's rules. Round-tripped through
    // the writer so the two spellings of the marker cannot drift apart.
    let one = block_for("/repo/AGENTS.md", "use mise");
    let two = block_for("/repo/crates/api/AGENTS.md", "leave the bindings alone");
    let wire = format!("tool output\n\nsome header\n\n{one}\n\n{two}");

    assert_eq!(
        blocks_in(&wire),
        vec![
            ("/repo/AGENTS.md".to_string(), "use mise".len()),
            (
                "/repo/crates/api/AGENTS.md".to_string(),
                "leave the bindings alone".len()
            ),
        ]
    );
    assert!(
        blocks_in("an ordinary tool result with no rules in it").is_empty(),
        "and nothing is found where nothing was written"
    );
}

#[test]
fn the_block_names_every_file_and_says_what_it_is() {
    // The model has to be able to tell a repository's standing rules from the
    // task, and to know which of the two wins when they disagree.
    let dir = tree(&[("AGENTS.md", "Use mise, never raw cargo.\n")]);
    let loaded = walk(&dir, ".").expect("the walk resolves");
    let block = loaded.block().expect("something was loaded");

    assert!(block.starts_with(PREAMBLE), "{block}");
    assert!(block.contains("takes precedence"), "{block}");
    assert!(block.contains("Contents of "), "{block}");
    assert!(block.contains("AGENTS.md"), "the path is named: {block}");
    assert!(block.ends_with("Use mise, never raw cargo."), "{block}");
}

#[test]
fn surrounding_blank_lines_are_trimmed_off_each_file() {
    // Every editor writes a trailing newline, and the seam between blocks is one
    // blank line. A file must not turn that into three.
    let dir = tree(&[
        ("AGENTS.md", "\n\nrule one\n\n\n"),
        ("CLAUDE.md", "rule two\n"),
    ]);
    let block = walk(&dir, ".")
        .expect("the walk resolves")
        .block()
        .expect("something was loaded");
    assert!(!block.contains("\n\n\n"), "{block:?}");
    assert!(block.ends_with("rule two"), "{block:?}");
}
