//! The project's own standing instructions, read from the checkout at startup.
//!
//! afi's prompt is compiled in and a supplied one is a file the operator names,
//! so the only way to give a run a repository's conventions was to paste them
//! into one of those. A pasted copy drifts from the original with nothing to
//! detect it: no import, no checksum, no failure when the upstream rules change,
//! and a reviewer applying last month's policy looks exactly like one that is
//! working. Reading the files from the checkout turns the copy into a reference,
//! so the rules are whatever the repository currently says.
//!
//! **Nothing is read unless the run asks for it.** These files are written by
//! whoever wrote the repository, and on a review job that repository is the thing
//! under review - a pull request editing `AGENTS.md` would be rewriting the
//! instructions of the agent reviewing it. So the walk is a setting the operator
//! turns on, the matching config key is theirs alone (see `super::file`'s
//! `Scope`), and a job that needs a fixed rule set names the files instead, from
//! a path the reviewed branch cannot reach. A run that configures nothing sends
//! the bytes afi has always sent.
//!
//! The walk itself is the shape both Claude Code and Codex settled on: read every
//! file at session start, broadest scope first, concatenate rather than merge, and
//! wrap each one in a header naming the file it came from. Nothing here is a model
//! feature - the model never opens a file - so all of it is decided by what gets
//! pasted into the request and in what order.
//!
//! The text is assembled once here and held for the whole run, which is what
//! keeps it inside the Anthropic cache prefix - see `crate::prompt`.

use std::collections::{HashMap, HashSet};
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::iter;
use std::path::{Path, PathBuf};

use crate::sessions::afi_home;
use crate::util;

use super::system_prompt::unreadable;

pub mod nested;

/// The file names a walk looks for in each directory, `$AFI_HOME` included.
///
/// Both, when a directory holds both. A `CLAUDE.md` that only points at the
/// `AGENTS.md` beside it costs a line of duplicated text; one that says something
/// of its own and got dropped for sharing a directory would be the silent loss
/// this whole setting exists to avoid.
const NAMES: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];

/// The value that turns the walk on.
const DISCOVER: &str = "project";

/// The value that turns everything off, for a run under an operator file or a
/// workflow env block that turned it on.
const OFF: &str = "none";

/// The most instruction text a run may load, across every file together.
///
/// These bytes sit in front of every request and the whole history is resent each
/// turn, so a large file is paid for on every one of them - roughly 8k tokens at
/// this cap. Over it the run refuses rather than sending part of a rule set:
/// half of `AGENTS.md` is instructions nobody wrote, and truncating quietly is
/// how a reviewer comes to enforce a policy that stops mid-sentence.
///
/// 32 KiB is what Codex caps its own `AGENTS.md` chain at, and matching it means a
/// repository that fits one tool's budget fits this one.
const MAX_BYTES: usize = 32 * 1024;

/// What the model is told these blocks are.
///
/// It states the precedence rather than leaving it to position, because the run
/// that most wants these files is also the one that supplied its own prompt, and
/// "whatever came last" is not the answer there.
const PREAMBLE: &str = "The blocks below are standing instructions for this project, \
     read from disk when this run started. Follow them while working in this project. \
     Anything above takes precedence, as does anything the user asks for directly. \
     Where two blocks disagree the later one wins - it was found closer to the work.";

/// One file's text, and the path it is reported and shown under.
#[derive(Debug, Clone)]
struct Loaded {
    path: String,
    text: String,
}

/// The instruction files a run loads, in the order their text is sent.
#[derive(Debug, Clone, Default)]
pub(super) struct Instructions {
    loaded: Vec<Loaded>,
    /// Where the list came from, which decides one thing beyond how an empty file is
    /// answered: whether a file deeper in the tree may arrive mid-session - see
    /// [`nested`]. Stored rather than reduced to a `walked` flag, because a derived
    /// copy of one bit is a copy that can disagree, and one did.
    found: Found,
}

impl Instructions {
    /// The system content these files add, or `None` when there are none.
    ///
    /// Each block names the file it came from, so a model asked why it followed a
    /// rule can say which file states it, and an operator reading the request can
    /// tell a repository's rule from afi's own.
    pub(super) fn block(&self) -> Option<String> {
        if self.loaded.is_empty() {
            return None;
        }
        let mut out = String::from(PREAMBLE);
        for file in &self.loaded {
            // Infallible: a String write cannot fail.
            let _ = write!(out, "\n\n{}", block_for(&file.path, &file.text));
        }
        Some(out)
    }

    /// Whether a walk found these, so a subtree file may still arrive mid-session.
    ///
    /// True for a walk that found nothing as well as one that found files: the run
    /// asked to read the tree, and a repository whose rules live only in a subtree
    /// has nothing to offer at startup and everything to offer later.
    pub(super) fn walked(&self) -> bool {
        self.found == Found::Discovered
    }

    /// What was loaded, as `(path, bytes sent)` in the order it was sent.
    ///
    /// The size is what this run put in front of the model, not what the file
    /// holds now - the two differ the moment somebody edits it mid-session, and the
    /// first is the only one that answers "why is it ignoring my rule".
    pub(super) fn files(&self) -> Vec<(String, usize)> {
        self.loaded
            .iter()
            .map(|file| (file.path.clone(), file.text.len()))
            .collect()
    }
}

/// Where a list of paths came from, which decides what an empty file means.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Found {
    /// Paths the run named, so each one is an instruction to send that file. The
    /// default, so a run that configured nothing reads no further file either.
    #[default]
    Named,
    /// Paths a walk turned up, so one with nothing in it is a file with nothing
    /// in it rather than a mistake.
    Discovered,
}

/// Resolve the instruction files this run loads from an already-merged setting.
///
/// `cwd` is where the walk starts; `None` is the process's own directory. `env`
/// supplies `AFI_HOME`, which is where the operator's own file lives. Tests pass a
/// temporary tree and a temporary home, which is what keeps them from reading the
/// checkout they are running inside or the developer's own standing instructions.
///
/// # Errors
///
/// Returns why the run must not start: a named file that could not be read, does
/// not exist, or says nothing; a value that names no file at all; a walk with no
/// working directory to start from; or more instruction text than [`MAX_BYTES`].
/// A walk that finds nothing is not among them - see [`discover`]. Never loads
/// part of what was asked for -
/// a run told to follow a repository's rules and following half of them is worse
/// than one that stops and says so.
pub(super) fn resolve(
    setting: Option<&str>,
    cwd: Option<&Path>,
    env: &HashMap<String, String>,
) -> Result<Instructions, String> {
    let Some(raw) = util::nonblank(setting) else {
        return Ok(Instructions::default());
    };
    if raw.eq_ignore_ascii_case(OFF) {
        return Ok(Instructions::default());
    }
    if raw.eq_ignore_ascii_case(DISCOVER) {
        return load(&discover(cwd, env)?, Found::Discovered);
    }
    // Named paths from here on. They are not a walk, so nothing deeper in the tree
    // arrives later either: pinning what a job sends is the whole point of naming.
    let paths = named(raw);
    if paths.is_empty() {
        return Err(format!(
            "the instructions setting {raw:?} names no file (want {DISCOVER}, {OFF}, \
             or a comma-separated list of paths)"
        ));
    }
    load(&paths, Found::Named)
}

/// Every instruction file the walk turns up, broadest first: the operator's own
/// `$AFI_HOME` file, then the project chain from its root down to `cwd`.
///
/// Broadest first is what makes a narrower file win, since the blocks are sent in
/// this order and both file names document themselves that way. The operator's
/// file leads because it applies to every project, so a repository's answer reads
/// after it.
///
/// The project chain stops at the directory holding `.git`, and outside a
/// repository it reads only `cwd`, which is the boundary the project config file
/// uses and for the same reason: with nothing to stop at, the walk reaches `$HOME`
/// and reads whatever standing instructions live up there as though this project
/// had written them.
///
/// A tree with no instruction file in it is an empty list rather than a refusal -
/// most repositories have none, and this is a walk, not a list of files someone
/// named. Not being able to tell where to walk *is* a refusal: a run asked for a
/// project's rules and loading none of them would be the silent drop.
///
/// # Errors
///
/// When `cwd` is `None` and the process has no readable working directory.
fn discover(cwd: Option<&Path>, env: &HashMap<String, String>) -> Result<Vec<PathBuf>, String> {
    let here = match cwd {
        Some(path) => path.to_path_buf(),
        None => env::current_dir().map_err(|error| {
            format!("can't tell which project to read instructions from: {error}")
        })?,
    };
    let stop = super::file::git_root(&here).unwrap_or_else(|| here.clone());
    let home = afi_home(env);
    let mut seen = HashSet::new();
    Ok(files_in(
        iter::once(home.as_path()).chain(chain_up(&here, &stop)),
        &mut seen,
    ))
}

/// `from` and every directory above it up to and including `stop`, broadest first.
///
/// The order is the precedence contract both walks promise: blocks are sent in this
/// order, so a directory closer to the work reads later and wins. `stop` has to be
/// an ancestor of `from` - both callers pass one - or the climb runs to the
/// filesystem root.
pub(super) fn chain_up<'a>(from: &'a Path, stop: &Path) -> Vec<&'a Path> {
    let mut chain: Vec<&Path> = Vec::new();
    let mut at = Some(from);
    while let Some(dir) = at {
        chain.push(dir);
        if dir == stop {
            break;
        }
        at = dir.parent();
    }
    chain.reverse();
    chain
}

/// The instruction files these directories hold, in order, each one once, marking
/// every one it returns in `seen`.
///
/// `seen` is the caller's, because the two walks mean different things by it. The
/// startup walk passes an empty set so that a file reached twice is sent once -
/// `AFI_HOME` at `~/.afi` with the working directory there finds it as both, and
/// sending it twice would pay for it twice on every request. The subtree walk passes
/// what the run has already sent, so a directory is read once per session. Marking
/// here rather than in the caller is what keeps one canonicalization per file.
pub(super) fn files_in<'a>(
    dirs: impl Iterator<Item = &'a Path>,
    seen: &mut HashSet<PathBuf>,
) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for path in dirs.flat_map(|dir| NAMES.map(|name| dir.join(name))) {
        if !path.is_file() {
            continue;
        }
        let key = path.canonicalize().unwrap_or_else(|_| path.clone());
        if seen.insert(key) {
            found.push(path);
        }
    }
    found
}

/// The blocks a message's text carries, as `(path, bytes)`.
///
/// The reverse of [`block_for`], and the only way to tell that a replayed tool result
/// already holds a project's rules - the text on the wire is all a resumed run has to
/// go on. Beside the writer so the two spellings of the marker cannot drift.
pub(super) fn blocks_in(text: &str) -> Vec<(String, usize)> {
    let mut found = Vec::new();
    for start in text.match_indices(MARKER).map(|(at, _)| at) {
        let rest = &text[start + MARKER.len()..];
        let Some(head) = rest.find(":\n\n") else {
            continue;
        };
        let body = &rest[head + 3..];
        // A block runs to the next one, or to the end of the message.
        let len = body.find(MARKER).map_or(body.len(), |next| {
            body[..next].trim_end_matches(char::is_whitespace).len()
        });
        found.push((rest[..head].to_string(), len));
    }
    found
}

/// One file's block, as the model reads it.
///
/// The one place the shape is written down, because both walks emit it and a model
/// asked which file states a rule can only answer from this line.
pub(super) fn block_for(path: &str, text: &str) -> String {
    format!("{MARKER}{path}:\n\n{text}")
}

/// What every block opens with, and what [`blocks_in`] finds it by.
const MARKER: &str = "Contents of ";

/// The paths a comma-separated setting names, in the order it wrote them.
///
/// Commas only. Every other list afi reads also separates on spaces, and these
/// are paths - a directory with a space in it is ordinary, and splitting on one
/// would turn a real file into two that do not exist.
fn named(raw: &str) -> Vec<PathBuf> {
    raw.split(',')
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// What `path` weighs on disk, or `0` when that cannot be read - in which case the
/// read that follows reports the real error rather than a guess about the size.
pub(super) fn file_size(path: &Path) -> usize {
    fs::metadata(path).map_or(0, |meta| usize::try_from(meta.len()).unwrap_or(usize::MAX))
}

/// Read every path, in order, or say why the run must not start.
fn load(paths: &[PathBuf], found: Found) -> Result<Instructions, String> {
    let mut loaded: Vec<Loaded> = Vec::new();
    let mut total = 0usize;
    for path in paths {
        let shown = path.display().to_string();
        // Weighed before it is read. The cap exists because these bytes are paid for
        // on every request, and pulling a multi-gigabyte file into memory in order to
        // refuse it spends far more than the cap was protecting. Trimming can only
        // shrink a file, so a chain that passes this check cannot exceed the cap once
        // read - which is why nothing re-checks the total afterwards.
        let weight = total.saturating_add(file_size(path));
        if weight > MAX_BYTES {
            return Err(over_cap(&loaded, &shown, weight));
        }
        let body = fs::read_to_string(path).map_err(|error| {
            format!("can't read the project instructions from {shown:?}: {error}")
        })?;
        let text = body.trim_matches(unreadable).to_string();
        if text.is_empty() {
            if found == Found::Named {
                return Err(format!("the project instructions at {shown:?} are empty"));
            }
            // A file the walk turned up that holds nothing adds nothing, and it
            // is left out of the reported paths, which is the report.
            continue;
        }
        total += text.len();
        loaded.push(Loaded { path: shown, text });
    }
    Ok(Instructions { loaded, found })
}

/// Why too much instruction text refuses the run, naming what it added up to and the
/// file that put it over.
fn over_cap(loaded: &[Loaded], offender: &str, total: usize) -> String {
    let names: Vec<&str> = loaded
        .iter()
        .map(|file| file.path.as_str())
        .chain(iter::once(offender))
        .collect();
    format!(
        "the project instructions are {total} bytes, over the {MAX_BYTES}-byte cap \
         ({}) - they ride in front of every request, so name the files this run \
         needs with --instructions <path,...>",
        names.join(", ")
    )
}

#[cfg(test)]
mod tests;
