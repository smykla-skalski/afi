//! Tests for the `--version` report.

use std::env;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::{BuildInfo, VERSION, file_digest, report};

/// A fully-populated build, so a test that cares about one missing field can
/// clear just that one.
fn build() -> BuildInfo {
    BuildInfo {
        commit: "eab8568c0b1f2d3e4f5a6b7c8d9e0f1a2b3c4d5e",
        commit_date: "2026-08-06T15:25:28+02:00",
        dirty: false,
        target: "x86_64-unknown-linux-musl",
        rustc: "1.97.1",
        profile: "release",
    }
}

/// Write `contents` to a file in `dir` and return its path.
fn file_with(dir: &TempDir, name: &str, contents: &[u8]) -> PathBuf {
    let path = dir.path().join(name);
    let mut file = fs::File::create(&path).expect("file must be creatable");
    file.write_all(contents).expect("contents must write");
    path
}

/// The label of every line, so a consumer's `grep sha256:` keeps working.
fn labels(report: &str) -> Vec<String> {
    report
        .lines()
        .skip(1)
        .filter_map(|line| line.trim().split(':').next())
        .map(str::to_string)
        .collect()
}

#[test]
fn report_leads_with_the_crate_version() {
    let out = report(&build(), None);
    let first = out.lines().next().expect("report must have a first line");
    assert_eq!(first, format!("afi {VERSION}"));
    // The version has to be the real crate version, not a placeholder: reporting
    // one that does not match the release it came from is the whole bug here.
    assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    assert!(!VERSION.is_empty(), "version must not be empty");
}

#[test]
fn report_names_every_field() {
    let out = report(&build(), None);
    assert_eq!(
        labels(&out),
        [
            "commit",
            "commit-date",
            "target",
            "profile",
            "rustc",
            "executable",
            "sha256",
        ]
    );
}

#[test]
fn report_shows_the_build_facts() {
    let out = report(&build(), None);
    assert!(
        out.contains("eab8568c0b1f2d3e4f5a6b7c8d9e0f1a2b3c4d5e"),
        "{out}"
    );
    assert!(out.contains("2026-08-06T15:25:28+02:00"), "{out}");
    assert!(out.contains("x86_64-unknown-linux-musl"), "{out}");
    assert!(out.contains("1.97.1"), "{out}");
    assert!(out.contains("release"), "{out}");
}

#[test]
fn a_dirty_build_says_so() {
    let mut info = build();
    info.dirty = true;
    let out = report(&info, None);
    // Without the marker the sha claims to describe code that was never compiled.
    assert!(
        out.contains("eab8568c0b1f2d3e4f5a6b7c8d9e0f1a2b3c4d5e (dirty)"),
        "{out}"
    );
}

#[test]
fn a_clean_build_is_not_marked_dirty() {
    let out = report(&build(), None);
    assert!(!out.contains("dirty"), "{out}");
}

#[test]
fn missing_build_facts_read_as_unknown() {
    let info = BuildInfo {
        commit: "",
        commit_date: "",
        dirty: false,
        target: "",
        rustc: "",
        profile: "release",
    };
    let out = report(&info, None);
    // A build with no git repository must still print a usable report.
    assert_eq!(out.matches("unknown").count(), 6, "{out}");
    assert!(out.starts_with(&format!("afi {VERSION}\n")), "{out}");
}

#[test]
fn the_digest_is_of_the_named_executable() {
    let dir = TempDir::new().expect("tempdir must be creatable");
    let path = file_with(&dir, "afi", b"abc");
    let out = report(&build(), Some(&path));
    // sha256("abc"), the published test vector.
    assert!(
        out.contains("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
        "{out}"
    );
    assert!(out.contains(&path.display().to_string()), "{out}");
}

#[test]
fn digest_matches_the_sha256_test_vectors() {
    let dir = TempDir::new().expect("tempdir must be creatable");
    let cases = [
        (
            &b""[..],
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        ),
        (
            &b"abc"[..],
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        ),
    ];
    for (index, (contents, expected)) in cases.into_iter().enumerate() {
        let path = file_with(&dir, &format!("case-{index}"), contents);
        assert_eq!(
            file_digest(&path).expect("digest must compute"),
            expected,
            "case {index}"
        );
    }
}

#[test]
fn a_large_executable_hashes_in_one_pass() {
    let dir = TempDir::new().expect("tempdir must be creatable");
    // Bigger than any single read buffer, so a chunked hash that forgot to feed
    // every chunk would disagree with the one-shot digest below.
    let contents = vec![b'x'; 3 * 1024 * 1024];
    let path = file_with(&dir, "afi", &contents);
    // An independent oracle rather than sha2 hashing the same bytes again, so this
    // still catches a chunking bug if the hashing crate itself is the thing at fault.
    assert_eq!(
        file_digest(&path).expect("digest must compute"),
        "3bea8a9a07c1e8dcaa4c1b816815c35a29b4fb585ba6ecc70ea44840a794cfb3"
    );
}

#[test]
fn an_unreadable_executable_reports_why() {
    let dir = TempDir::new().expect("tempdir must be creatable");
    let missing = dir.path().join("gone");
    let out = report(&build(), Some(&missing));
    // Reported, not dropped: a missing sha256 line would look like a build that
    // predates digests rather than a path that could not be read.
    assert!(out.contains("sha256:"), "{out}");
    assert!(out.contains("unavailable ("), "{out}");
    // The rest of the report still has to arrive.
    assert!(out.contains("x86_64-unknown-linux-musl"), "{out}");
}

#[test]
fn an_unknown_executable_path_is_not_a_panic() {
    let out = report(&build(), None);
    assert!(out.contains("executable:  unknown"), "{out}");
    assert!(out.contains("sha256:      unknown"), "{out}");
}

#[test]
fn every_line_is_a_single_label_value_pair() {
    let out = report(&build(), None);
    assert!(out.ends_with('\n'), "report must end with a newline");
    for line in out.lines().skip(1) {
        let (label, value) = line
            .trim_end()
            .split_once(": ")
            .unwrap_or_else(|| panic!("not a label/value line: {line:?}"));
        assert!(label.starts_with("  "), "field must be indented: {line:?}");
        assert!(
            !value.trim().is_empty(),
            "field must have a value: {line:?}"
        );
    }
}

#[test]
fn this_binarys_own_build_info_is_populated() {
    let info = BuildInfo::current();
    // Guards the build script: if `env!` were wired to the wrong key, or the probes
    // silently stopped running in this repository, these would be empty.
    assert!(!info.target.is_empty(), "target must come from build.rs");
    assert!(!info.rustc.is_empty(), "rustc must come from build.rs");
    assert_eq!(
        info.profile,
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );
    assert!(
        info.commit.len() == 40 && info.commit.chars().all(|c| c.is_ascii_hexdigit()),
        "commit must be a full sha in a git checkout, got {:?}",
        info.commit
    );
}

#[test]
fn the_running_binary_digests_itself() {
    // `report_current` is what `--version` actually calls, and the test binary is a
    // real executable, so this covers the `current_exe` path end to end.
    let out = super::report_current();
    let digest = out
        .lines()
        .find_map(|line| line.trim().strip_prefix("sha256:"))
        .expect("report must carry a digest")
        .trim();
    assert_eq!(
        digest.len(),
        64,
        "digest must be sha256 hex, got {digest:?}"
    );
    assert!(digest.chars().all(|c| c.is_ascii_hexdigit()), "{digest:?}");

    let exe = env::current_exe().expect("test binary must have a path");
    assert_eq!(digest, file_digest(&exe).expect("digest must compute"));
    assert!(
        digest_is_of_a_real_file(&exe),
        "digest must be of a real file"
    );
}

fn digest_is_of_a_real_file(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|meta| meta.len() > 0)
}
