//! Writing the summary to a path, and proving the path can take it.
//!
//! Capturing stdout to get the JSON costs the readable rendering of the run, and
//! it puts the only machine copy behind a pipe that a wrapper, a tee, or a shell
//! printing one line of its own can corrupt. A path is addressed rather than
//! piped, so a workflow can upload it as a build artifact and leave stdout to
//! the human view.
//!
//! Split from `summary.rs` so the object and the channel stay apart: what a run
//! reports is one contract, and landing one complete copy of it where a build
//! step will collect it is another.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::atomic;
use crate::util;

/// The path the summary is also written to, from `--summary-file` /
/// `AFI_SUMMARY_FILE`.
///
/// Independent of `SummaryFormat`: naming a file does not turn `--summary json`
/// on. Leaving stdout to the rendered run is the reason to ask for a file at
/// all, so implying the stdout copy would take back the readable output the
/// caller kept. Pass both to get both.
///
/// A blank value is no path, matching how the other variables here read a shell
/// variable that is exported but unset - see `util::nonblank`. The flag is
/// stricter, because writing it out is a statement that a file is wanted.
#[must_use]
pub fn summary_path(raw: Option<&str>) -> Option<PathBuf> {
    util::nonblank(raw).map(PathBuf::from)
}

/// Prove the summary can reach `path` before the run starts.
///
/// Creates and removes the very temp file the real write will use, so a missing
/// directory, one that cannot be written, or a path that is itself a directory
/// is reported in a second rather than after a run has been paid for. Nothing is
/// left behind and the target is untouched, so a summary from a previous run
/// stays readable until this one has a complete object to replace it with.
///
/// # Errors
///
/// Returns why the path cannot be written, naming it.
pub fn writable(path: &Path) -> Result<(), String> {
    if let Some(problem) = directory_problem(path) {
        return Err(format!(
            "can't write the run summary to {}: {problem}",
            path.display()
        ));
    }
    match atomic::create_temp(path) {
        Ok((probe, _)) => {
            let _ = fs::remove_file(&probe);
            Ok(())
        }
        Err(error) => Err(reason(path, &error)),
    }
}

/// Why `path` names a directory rather than a file, if it does.
///
/// Neither case survives to the write, and neither is caught by creating a temp
/// sibling: the sibling of a directory is an ordinary name that opens fine, and
/// only the rename at the end of the run would fail. Checking both here is what
/// moves the failure to before the run is paid for.
///
/// The trailing separator is the case a caller reaches by accident, from
/// `--summary-file "$OUTDIR/$NAME"` with `NAME` unset. `file_name` strips the
/// separator, so the path looks like an ordinary file to everything downstream
/// until `rename` refuses it.
fn directory_problem(path: &Path) -> Option<&'static str> {
    if path.is_dir() {
        return Some("it is a directory");
    }
    if path.as_os_str().as_encoded_bytes().last() == Some(&b'/') {
        return Some("it names a directory, not a file");
    }
    None
}

/// Write `summary` to `path` as one line of JSON.
///
/// Goes through a temp sibling and a rename, so a reader that opens the path
/// sees either nothing or one complete object - never the prefix of one still
/// being written. See `crate::atomic` for why the temp file is opened the way
/// it is.
///
/// # Errors
///
/// Returns why the path could not be written. The caller fails the run on it
/// rather than falling back to stdout, which would be no fallback at all: a
/// caller that asked for a file is not watching stdout for the JSON.
pub fn write_file(path: &Path, summary: &Value) -> Result<(), String> {
    // Trailing newline so the file reads like every other line-oriented artifact
    // a workflow collects, and so `read` in a shell loop terminates.
    let body = format!("{summary}\n");
    atomic::write(path, body.as_bytes()).map_err(|error| reason(path, &error))
}

fn reason(path: &Path, error: &io::Error) -> String {
    format!("can't write the run summary to {}: {error}", path.display())
}

#[cfg(test)]
mod tests;
