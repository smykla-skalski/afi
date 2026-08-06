//! Build-time metadata for `afi --version`.
//!
//! Every probe here is best-effort. A build from a release tarball, from
//! crates.io, or in a container with the sources copied in has no git repository
//! and may have no `git` binary at all, so each probe degrades to an empty string
//! and `src/version.rs` renders it as "unknown". None of this can fail a build:
//! a coding agent that will not compile because it cannot describe itself would
//! be a poor trade.

use std::env;
use std::path::Path;
use std::process::Command;

fn main() {
    emit("AFI_BUILD_TARGET", &var("TARGET").unwrap_or_default());
    emit("AFI_BUILD_RUSTC", &rustc_version().unwrap_or_default());

    // An explicit override wins so a build with no `.git` can still name the
    // commit it came from. `GITHUB_SHA` covers CI without a workflow change.
    let commit = var("AFI_BUILD_COMMIT")
        .or_else(|| var("GITHUB_SHA"))
        .or_else(|| git(&["rev-parse", "HEAD"]))
        .unwrap_or_default();
    emit("AFI_BUILD_COMMIT", &commit);

    // The commit's own date, not the wall clock. Cargo caches build-script output,
    // so a "built at" stamp would record when this script last ran and drift away
    // from the binary beside it; a commit date is stable and reproducible.
    //
    // Overridable for the same reason as the commit: a build in a container that
    // carries the sources but no `git` has no other way to supply it.
    let date = var("AFI_BUILD_COMMIT_DATE")
        .or_else(|| git(&["log", "-1", "--format=%cI"]))
        .unwrap_or_default();
    emit("AFI_BUILD_COMMIT_DATE", &date);

    // Tracked modifications only. Counting untracked files would mark nearly every
    // working copy dirty, since a scratch file is not part of the build, while a
    // modified tracked file does mean the binary is not the commit named above.
    let dirty =
        git(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|out| !out.is_empty());
    emit("AFI_BUILD_DIRTY", if dirty { "1" } else { "" });

    watch_inputs();
}

/// Set a compile-time environment variable readable through `env!`.
fn emit(key: &str, value: &str) {
    println!("cargo::rustc-env={key}={value}");
}

fn var(key: &str) -> Option<String> {
    env::var(key).ok().filter(|v| !v.is_empty())
}

/// Run `git` in the package directory, returning trimmed stdout on success.
fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    Some(text.trim().to_string())
}

/// The `release:` field of `rustc -vV`, so the report shows `1.97.1` rather than
/// the whole version banner.
fn rustc_version() -> Option<String> {
    let rustc = var("RUSTC").unwrap_or_else(|| "rustc".to_string());
    let out = Command::new(rustc).arg("-vV").output().ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    let release = text
        .lines()
        .find_map(|line| line.strip_prefix("release:"))?;
    Some(release.trim().to_string())
}

/// Declare what makes this script's output stale.
///
/// Emitting any `rerun-if-changed` replaces cargo's default of "rerun when any
/// file in the package changed", so the package inputs have to be listed here
/// alongside the git state that cargo knows nothing about.
fn watch_inputs() {
    for path in ["src", "Cargo.toml", "Cargo.lock", "build.rs"] {
        println!("cargo::rerun-if-changed={path}");
    }
    for key in [
        "AFI_BUILD_COMMIT",
        "AFI_BUILD_COMMIT_DATE",
        "GITHUB_SHA",
        "RUSTC",
    ] {
        println!("cargo::rerun-if-env-changed={key}");
    }

    // `git commit` moves the branch ref without touching a single tracked file, so
    // watching the sources alone would leave the recorded commit one behind. In a
    // linked worktree HEAD and the refs live in different directories, which is why
    // both are resolved rather than assuming `.git/`.
    let Some(gitdir) = git(&["rev-parse", "--absolute-git-dir"]) else {
        return;
    };
    watch(&Path::new(&gitdir).join("HEAD"));

    let Some(common) = git(&["rev-parse", "--git-common-dir"]) else {
        return;
    };
    let common = Path::new(&common);
    watch(&common.join("packed-refs"));
    // Absent on a detached HEAD, where `<gitdir>/HEAD` above already carries the
    // commit and there is no ref to follow.
    if let Some(refname) = git(&["symbolic-ref", "--quiet", "HEAD"]) {
        watch(&common.join(refname));
    }
}

/// Watch `path`, but only if it exists: naming a missing file tells cargo the
/// script is stale on every single build.
fn watch(path: &Path) {
    if path.exists() {
        println!("cargo::rerun-if-changed={}", path.display());
    }
}
