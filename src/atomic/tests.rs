use super::*;

use std::collections::HashSet;
use std::os::unix::fs::symlink;

use tempfile::TempDir;

#[test]
fn the_body_lands_whole_and_replaces_what_was_there() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("out.json");

    write(&path, b"first").unwrap();
    write(&path, b"second").unwrap();

    assert_eq!(fs::read_to_string(&path).unwrap(), "second");
}

#[test]
fn nothing_is_left_beside_the_target() {
    // A workflow collecting the directory should not find a second file, and a
    // reader polling the target must never see a partial one.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("out.json");

    write(&path, b"body").unwrap();

    let left: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(left, vec![OsString::from("out.json")]);
}

#[test]
fn a_planted_symlink_at_the_temp_name_is_refused_not_followed() {
    // The attack this file exists to stop. Another local user who can write the
    // directory pre-creates the temp name pointing somewhere else; a plain
    // `File::create` would truncate the victim and then write our bytes into it
    // with our permissions. `create_new` fails on the existing entry instead.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("out.json");
    let victim = dir.path().join("victim");
    fs::write(&victim, "do not touch").unwrap();

    // Plant every name `create_temp` could pick, by planting the one it is about
    // to pick: take a name, remove the file, and leave a symlink there.
    let (planted, _) = create_temp(&path).unwrap();
    fs::remove_file(&planted).unwrap();
    symlink(&victim, &planted).unwrap();

    // A fresh name is drawn, so this write succeeds without going near the link.
    write(&path, b"summary").unwrap();
    assert_eq!(fs::read_to_string(&victim).unwrap(), "do not touch");
    assert_eq!(fs::read_to_string(&path).unwrap(), "summary");

    // And opening the planted name itself is refused rather than followed.
    let error = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&planted)
        .expect_err("an existing symlink must not be opened");
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(fs::read_to_string(&victim).unwrap(), "do not touch");
}

#[test]
fn each_temp_name_differs_so_a_name_cannot_be_predicted_or_collided_with() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("out.json");
    let names: HashSet<_> = (0..64).map(|_| temp_path(&path)).collect();
    assert_eq!(names.len(), 64, "temp names must not repeat");
}

#[test]
fn the_temp_name_is_a_sibling_of_the_target() {
    // `rename` is only atomic within one filesystem, so the temp file cannot
    // live in the system temp directory.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("out.json");
    assert_eq!(temp_path(&path).parent(), path.parent());
}

#[test]
fn a_missing_directory_fails_and_leaves_nothing() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("no-such-dir/out.json");

    let error = write(&path, b"body").expect_err("a missing directory must fail");

    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    assert!(!path.exists());
}

#[test]
fn a_failed_write_leaves_an_existing_target_alone() {
    // The target is only ever replaced by a rename, so a run that could not
    // produce a whole file must leave the previous one readable.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("out.json");
    fs::write(&path, "previous").unwrap();

    // A directory in place of the target: the temp file writes fine, the rename
    // is what fails.
    let onto_dir = dir.path().join("adir");
    fs::create_dir(&onto_dir).unwrap();
    assert!(write(&onto_dir, b"body").is_err());

    assert_eq!(fs::read_to_string(&path).unwrap(), "previous");
    let left: Vec<_> = fs::read_dir(&onto_dir).unwrap().collect();
    assert!(left.is_empty(), "the temp file must be cleared up");
}
