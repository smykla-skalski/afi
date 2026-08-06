//! What `afi --version` reports: the crate version, the build it came from, and
//! the digest of the executable that is running.
//!
//! The build facts arrive from `build.rs` through `env!` and are all best-effort
//! (see that file). The digest is computed at runtime instead, because a binary
//! cannot contain its own hash.

use std::env;
use std::fmt::Write as _;
use std::fs::File;
use std::io;
use std::path::Path;

use sha2::{Digest, Sha256};

#[cfg(test)]
mod tests;

/// This build's crate version, e.g. `0.2.0`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Stands in for any fact `build.rs` could not determine.
const UNKNOWN: &str = "unknown";

/// Label column width in `report`, wide enough for the longest label.
const LABEL_WIDTH: usize = 12;

/// Facts fixed when this binary was compiled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildInfo {
    /// Full commit sha, or empty when it could not be determined.
    pub commit: &'static str,
    /// Committer date of `commit`, ISO 8601.
    pub commit_date: &'static str,
    /// Whether tracked files were modified relative to `commit`.
    pub dirty: bool,
    /// Target triple this was built for, e.g. `x86_64-unknown-linux-musl`.
    pub target: &'static str,
    /// Compiler release, e.g. `1.97.1`.
    pub rustc: &'static str,
    /// `release` or `debug`.
    pub profile: &'static str,
}

impl BuildInfo {
    /// The facts for this binary.
    #[must_use]
    pub const fn current() -> Self {
        Self {
            commit: env!("AFI_BUILD_COMMIT"),
            commit_date: env!("AFI_BUILD_COMMIT_DATE"),
            dirty: !env!("AFI_BUILD_DIRTY").is_empty(),
            target: env!("AFI_BUILD_TARGET"),
            rustc: env!("AFI_BUILD_RUSTC"),
            profile: if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            },
        }
    }
}

/// `report` for the running process.
#[must_use]
pub fn report_current() -> String {
    report(&BuildInfo::current(), env::current_exe().ok().as_deref())
}

/// Render the `--version` report.
///
/// `exe` is a parameter rather than read from `env::current_exe` so tests can
/// point it at a file whose digest is known. `None` means the path could not be
/// determined, which is reported rather than hidden.
///
/// The shape is one `label: value` per line so `afi --version | grep sha256:`
/// works without a JSON parser.
#[must_use]
pub fn report(build: &BuildInfo, exe: Option<&Path>) -> String {
    let commit = describe_commit(build);
    let (exe_path, digest) = describe_exe(exe);
    let fields = [
        ("commit", commit.as_str()),
        ("commit-date", or_unknown(build.commit_date)),
        ("target", or_unknown(build.target)),
        ("profile", build.profile),
        ("rustc", or_unknown(build.rustc)),
        ("executable", exe_path.as_str()),
        ("sha256", digest.as_str()),
    ];

    let width = LABEL_WIDTH;
    let mut out = format!("afi {VERSION}\n");
    for (label, value) in fields {
        let label = format!("{label}:");
        // Infallible: a String write cannot fail.
        let _ = writeln!(out, "  {label:<width$} {value}");
    }
    out
}

/// The commit, marked when the working tree it was built from had been modified:
/// without that marker the sha would claim to describe code that was never built.
fn describe_commit(build: &BuildInfo) -> String {
    if build.commit.is_empty() {
        return UNKNOWN.to_string();
    }
    if build.dirty {
        return format!("{} (dirty)", build.commit);
    }
    build.commit.to_string()
}

/// The executable's path and sha256.
///
/// A failure is reported in place rather than dropping the field. "Which binary is
/// this, exactly" is the question `--version` exists to answer, so a silent gap
/// would be worse than a stated reason.
fn describe_exe(exe: Option<&Path>) -> (String, String) {
    let Some(path) = exe else {
        return (UNKNOWN.to_string(), UNKNOWN.to_string());
    };
    let digest = file_digest(path).unwrap_or_else(|error| format!("unavailable ({error})"));
    (path.display().to_string(), digest)
}

/// The sha256 of `path` as lowercase hex.
///
/// Streamed rather than read whole: the release binaries are several megabytes and
/// there is no reason to hold one in memory to hash it.
fn file_digest(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    io::copy(&mut file, &mut hasher)?;
    let mut hex = String::with_capacity(64);
    for byte in hasher.finalize() {
        // Infallible: a String write cannot fail.
        let _ = write!(hex, "{byte:02x}");
    }
    Ok(hex)
}

fn or_unknown(value: &str) -> &str {
    if value.is_empty() { UNKNOWN } else { value }
}
