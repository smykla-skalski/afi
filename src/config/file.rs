//! Settings from a file: `$AFI_HOME/config.json`.
//!
//! Everything afi can be told is an `AFI_*` variable, which is a flat string
//! keyspace. Anything with structure has to be flattened into it: a source
//! becomes a set of variables whose names encode its name, and the price table
//! and the Anthropic extra body become JSON squeezed onto one line of shell.
//! Worse, a misspelled variable is skipped in silence, so a run starts with the
//! setting its operator thought they had set simply absent.
//!
//! This layer adds a second way in, and only a way in: a file's keys lower to
//! the same variable names, are folded into the same env map, and are read by
//! the same code an exported variable is read by. Nothing downstream knows a
//! file exists. That is what keeps the file from becoming a second definition of
//! what a setting means.
//!
//! Precedence is one rule, applied by filling gaps rather than by overwriting: a
//! flag beats a variable, a variable beats the file, and the file beats the
//! built-in default.
//!
//! A key nothing reads, or a value of the wrong shape, refuses the run rather
//! than being dropped. The whole point is that a setting written in the file
//! took effect, and a file layer that ignores what it does not recognize would
//! reproduce the silence it exists to end.
//!
//! A project keeps its own file, the nearest `.afi/config.json` at or above the
//! working directory, and it is trusted less than the operator's. That file is
//! written by whoever wrote the repository: given the whole keyspace, one key
//! redirecting a source's `base_url` is enough for a clone to receive whatever
//! credential the operator's environment holds, and `approval` in the same file
//! switches off the gate that would have asked. So the keyspace is split - see
//! [`schema::Scope`] - and a project file sets what a repository has a say in
//! while the rest stays with the operator. No config file holds a credential at
//! all.

use std::collections::HashMap;
use std::env;
use std::fs;
use std::hash::BuildHasher;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::sessions::afi_home;
use crate::util;

mod lower;
mod schema;
mod suggest;
mod value;

use schema::Merge;

#[cfg(test)]
mod tests;

/// The file's name, in `$AFI_HOME` and in a project's `.afi`.
const FILE_NAME: &str = "config.json";
/// The directory a project keeps its file in.
const PROJECT_DIR: &str = ".afi";
/// The variable naming one file in place of both defaults.
const CONFIG_ENV: &str = "AFI_CONFIG";

/// Where a config file came from, which decides what it may set.
///
/// The operator's own file, and one they name with `--config`, are the same
/// thing here: naming a path is the act of trust. A file found by walking up from
/// the working directory is not named by anyone, so it is the other kind - see
/// [`schema::Scope`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// A file the operator keeps, or one they named.
    Operator,
    /// A file found in the working tree.
    WorkingTree,
}

/// The files a run reads its settings from, lowest precedence first.
///
/// The operator's own `$AFI_HOME/config.json`, then the project's - so a
/// repository's answer wins, key by key, over the operator's for the keys it is
/// allowed to give.
///
/// `named` is `--config`; `AFI_CONFIG` stands in when the flag is absent, and
/// either replaces both defaults rather than joining them - a caller pointing at
/// one file means that file, and gets [`Origin::Operator`] for it, because
/// naming a path is the act of trust. A blank value names nothing and leaves the
/// defaults alone, which is what an exported-but-unset shell variable looks like.
///
/// A named path is returned without checking that it exists, because a path
/// someone typed and that holds no file is a mistake and starting anyway would
/// run with settings nobody chose. A default is returned only when it holds a
/// file, which is what leaves "no config file" as an ordinary run configured by
/// environment and flags.
///
/// `env` supplies `AFI_HOME`, so it wants the env map after any env file has been
/// merged - see `Runtime::resolve_env`.
#[must_use]
pub fn config_files<S: BuildHasher>(
    named: Option<&str>,
    env: &HashMap<String, String, S>,
    cwd: Option<&Path>,
) -> Vec<(PathBuf, Origin)> {
    if let Some(path) = util::nonblank(named.or_else(|| env.get(CONFIG_ENV).map(String::as_str))) {
        return vec![(PathBuf::from(path), Origin::Operator)];
    }
    let mut found = Vec::new();
    let user = afi_home(env).join(FILE_NAME);
    if user.is_file() {
        found.push((user, Origin::Operator));
    }
    // The same file reached two ways is read once: `$AFI_HOME` at `~/.afi` with
    // the working directory at `~` finds it as both, and the operator's trust is
    // the one to keep.
    if let Some(project) = project_file(cwd)
        && !found.iter().any(|(first, _)| same_file(first, &project))
    {
        found.push((project, Origin::WorkingTree));
    }
    found
}

/// The nearest `.afi/config.json` at or above `cwd`, bounded by the project.
///
/// The walk stops at the directory holding `.git`, so "the project's file" means
/// one inside the project rather than whatever the first ancestor happens to
/// hold. Outside a repository only `cwd` is checked, for the same reason: with no
/// boundary to stop at, the walk would eventually reach `$HOME` and pick the
/// operator's own file up a second time as though a project had written it - and
/// with less trust than it has.
fn project_file(cwd: Option<&Path>) -> Option<PathBuf> {
    let here = match cwd {
        Some(path) => path.to_path_buf(),
        None => env::current_dir().ok()?,
    };
    // No repository: `cwd` is both the start and the boundary.
    let stop = git_root(&here).unwrap_or_else(|| here.clone());
    let mut dir = Some(here.as_path());
    while let Some(at) = dir {
        let candidate = at.join(PROJECT_DIR).join(FILE_NAME);
        if candidate.is_file() {
            return Some(candidate);
        }
        if at == stop {
            return None;
        }
        dir = at.parent();
    }
    None
}

/// The nearest ancestor holding `.git`, which is a directory in a clone and a
/// file in a worktree - `exists` covers both.
fn git_root(from: &Path) -> Option<PathBuf> {
    from.ancestors()
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_path_buf)
}

/// Whether two paths are the same file, following links and `..` where the
/// filesystem can say so.
fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// What a run's config files say, lowered to the variable names every setting
/// already travels as.
#[derive(Debug, Clone, Default)]
pub struct FileSettings {
    values: HashMap<String, String>,
    refusals: Vec<String>,
}

impl FileSettings {
    /// Read every file, lowest precedence first.
    ///
    /// A later file's key replaces an earlier one's, key by key rather than file
    /// by file, so a project setting wins over the same setting in the operator's
    /// file while leaving the rest of it standing.
    #[must_use]
    pub fn load(files: &[(PathBuf, Origin)]) -> Self {
        let mut settings = Self::default();
        for (path, origin) in files {
            settings.read(path, *origin);
        }
        settings
    }

    /// Why the run must not start: a file that would not read, a key nothing
    /// reads, a value of the wrong shape. Empty when the file read cleanly, and
    /// when there was none.
    #[must_use]
    pub fn refusals(&self) -> &[String] {
        &self.refusals
    }

    /// Fill gaps in `env`, leaving anything already set alone - which is what
    /// makes a variable, and the flags written into the map before this, beat the
    /// file.
    ///
    /// A variable set to nothing counts as set, blank though it is. Several of
    /// these read a blank as a value rather than as an absence, and it is the
    /// value that turns the setting off: `AFI_SUMMARY_FILE=` names no file,
    /// `AFI_SYSTEM_PROMPT_FILE=` sends afi's own prompt, a blank source
    /// `API_KEY` sends no credential. Filling those would make the run do more
    /// than was asked for - write a file that was suppressed, send instructions
    /// that were switched off - so a blank keeps beating the file, as the
    /// precedence rule says on its face.
    ///
    /// A file with anything wrong in it applies nothing at all. The run is about
    /// to refuse to start, and a half-applied file is a worse thing to hand to a
    /// caller that ignores the refusal than an unapplied one.
    pub fn apply_to<S: BuildHasher>(&self, env: &mut HashMap<String, String, S>) {
        if !self.refusals.is_empty() {
            return;
        }
        for (name, value) in &self.values {
            env.entry(name.clone()).or_insert_with(|| value.clone());
        }
    }

    /// Read one file into `self`.
    fn read(&mut self, path: &Path, origin: Origin) {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => {
                self.refusals.push(format!("{}: {error}", path.display()));
                return;
            }
        };
        let lowered = lower::read(path, &text, origin);
        self.refusals.extend(lowered.refusals);
        for (name, value, how) in lowered.pairs {
            if let Err(why) = self.merge(name, value, how) {
                self.refusals.push(format!("{}: {why}", path.display()));
            }
        }
    }

    /// Take one variable from a file, combining it with what an earlier file said
    /// rather than replacing it where replacing could lose something.
    ///
    /// A later file wins, which for most settings is the whole rule: the project
    /// picks the model, and the operator's choice of model is what it replaces.
    /// Which settings are exceptions, and why each one is, belongs to the schema -
    /// see [`schema::Merge`]. This decides nothing, it applies what the key
    /// carried.
    ///
    /// # Errors
    ///
    /// When two files cannot be combined at all, which only an allow list with
    /// nothing in common can be.
    fn merge(&mut self, name: String, value: String, how: Merge) -> Result<(), String> {
        let combined = match (self.values.get(&name), how) {
            (Some(first), Merge::Union) => union(first, &value),
            (Some(first), Merge::Intersection) => intersection(first, &value)?,
            (Some(first), Merge::Either) => either_on(first, &value),
            (Some(first), Merge::Object) => objects(first, &value),
            (None, _) | (Some(_), Merge::Replace) => value,
        };
        self.values.insert(name, combined);
        Ok(())
    }
}

/// Two JSON objects, key by key, the later file's winning where both speak.
///
/// Only the top level: a key either file sets, it sets whole. That is the line
/// between "the other file said nothing about this model" - which must survive -
/// and "both said something about it", where one answer has to win and the later
/// file is the one that does.
///
/// Either side failing to parse leaves the later value alone. Both were written by
/// `value::object` from JSON that already parsed once, so this is unreachable
/// rather than tolerated; replacing is what the caller would have done anyway.
fn objects(first: &str, second: &str) -> String {
    let (Ok(Value::Object(mut base)), Ok(Value::Object(over))) = (
        serde_json::from_str::<Value>(first),
        serde_json::from_str::<Value>(second),
    ) else {
        return second.to_string();
    };
    base.extend(over);
    Value::Object(base).to_string()
}

/// Every name in either list. A longer deny list denies more.
fn union(first: &str, second: &str) -> String {
    let mut names: Vec<&str> = names(first).chain(names(second)).collect();
    names.sort_unstable();
    names.dedup();
    names.join(",")
}

/// Only the names in both lists, so neither file can add to what the other
/// permitted.
///
/// # Errors
///
/// When the two have no name in common, which is a conflict between two files
/// rather than a value either of them got wrong. It cannot be answered with an
/// empty list, because an empty list reads as "every tool" by the time it reaches
/// the policy - the run would end up with every tool precisely because two files
/// agreed on none. Reported here, where both lists are in hand and the reason can
/// be said, rather than through a placeholder name the tool registry would then
/// call a typo.
fn intersection(first: &str, second: &str) -> Result<String, String> {
    let mut both: Vec<&str> = names(first)
        .filter(|name| names(second).any(|other| other.eq_ignore_ascii_case(name)))
        .collect();
    both.sort_unstable();
    both.dedup();
    if both.is_empty() {
        return Err(format!(
            "allowed_tools names no tool the allow list already read permits \
             ({first}), so the two together permit nothing - name a tool both \
             carry, or take tools away with disallowed_tools or read_only"
        ));
    }
    Ok(both.join(","))
}

/// On when either file asks for it, since the posture only ever tightens.
fn either_on(first: &str, second: &str) -> String {
    let on = |raw: &str| {
        !matches!(
            raw.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "no" | "off"
        )
    };
    if on(first) || on(second) {
        "1".to_string()
    } else {
        "0".to_string()
    }
}

/// The names in one list value, however it was separated.
fn names(raw: &str) -> impl Iterator<Item = &str> {
    raw.split([',', ' ', '\t', '\n'])
        .map(str::trim)
        .filter(|name| !name.is_empty())
}
