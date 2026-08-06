//! Writing a file so a reader never sees half of one, and so another local user
//! cannot decide where the bytes land.
//!
//! Every write here goes to a temp sibling of the target, is flushed to disk,
//! and is renamed into place. A sibling rather than a system temp directory
//! because `rename` is only atomic within one filesystem.
//!
//! The temp file is opened with `O_CREAT|O_EXCL` (`create_new`), never plain
//! `create`. `create` truncates whatever it finds and follows a symlink, so a
//! predictable temp name in a directory a second local user can write - a shared
//! CI workspace, a mounted volume, `/tmp` - is an arbitrary-file truncate and
//! overwrite carrying the invoking user's permissions. `create_new` fails on an
//! existing entry instead of following it, which turns that from a silent
//! redirect into a reported error. The name also carries OS randomness, so a
//! planted name is a lost race rather than a standing block; the retry covers an
//! ordinary collision either way.

use std::collections::hash_map::RandomState;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::hash::{BuildHasher, Hasher};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process;

/// How many temp names to try before giving up. More than one because a name
/// can collide or be occupied; small because each retry draws a fresh random
/// suffix, so anything beyond a handful is not a collision but a directory that
/// cannot be written.
const ATTEMPTS: u8 = 8;

/// Write `body` to `path` through a temp sibling, then rename it into place.
///
/// # Errors
///
/// Returns the first I/O failure. The temp file is removed on the way out, so a
/// failed write leaves nothing behind and leaves any existing target untouched.
pub fn write(path: &Path, body: &[u8]) -> io::Result<()> {
    let (tmp, file) = create_temp(path)?;
    match finish(file, &tmp, path, body) {
        Ok(()) => Ok(()),
        Err(error) => {
            // The rename never happened, so the temp file is ours to clear up.
            // Best-effort: the write error is what the caller needs to hear.
            let _ = fs::remove_file(&tmp);
            Err(error)
        }
    }
}

fn finish(mut file: fs::File, tmp: &Path, path: &Path, body: &[u8]) -> io::Result<()> {
    file.write_all(body)?;
    file.sync_all()?;
    drop(file);
    fs::rename(tmp, path)
}

/// Create a fresh temp sibling of `path`, returning its path and open handle.
///
/// Exposed so a caller can prove a directory is writable without writing the
/// target: create, then remove. See the module comment for why this is
/// `create_new` rather than `create`.
///
/// # Errors
///
/// Returns the underlying failure, or `AlreadyExists` when every attempted name
/// was taken.
pub fn create_temp(path: &Path) -> io::Result<(PathBuf, fs::File)> {
    let mut taken = None;
    for _ in 0..ATTEMPTS {
        let tmp = temp_path(path);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
        {
            Ok(file) => return Ok((tmp, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => taken = Some(error),
            Err(error) => return Err(error),
        }
    }
    Err(taken.unwrap_or_else(|| io::Error::from(io::ErrorKind::AlreadyExists)))
}

/// A candidate temp name beside `path`, different on every call.
fn temp_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map_or_else(|| OsString::from("tmp"), OsStr::to_os_string);
    name.push(format!(".{}.{:016x}.tmp", process::id(), noise()));
    path.with_file_name(name)
}

/// A random value from the OS, without a `rand` dependency.
///
/// `RandomState` is seeded per process from the operating system and advances on
/// each construction, so successive calls differ. The safety of the write does
/// not rest on this - `create_new` is what refuses a planted name - so this only
/// has to make the name hard to occupy in advance.
fn noise() -> u64 {
    RandomState::new().build_hasher().finish()
}

#[cfg(test)]
mod tests;
