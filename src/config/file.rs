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
//! **Only files the operator named are read.** A per-project file - the nearest
//! `.afi/config.json` above the working directory - was built and then taken back
//! out, because it made every repository a configuration input: one key
//! redirecting a source's `base_url` was enough for a clone to receive whatever
//! credential `$NAME` indirection resolves out of the operator's own environment
//! or env file, and `approval` in the same file switched off the gate that would
//! have asked. Nothing else in afi reads configuration out of the working tree,
//! and this layer will not be the first without a trust decision to lean on.

use std::collections::HashMap;
use std::fs;
use std::hash::BuildHasher;
use std::path::{Path, PathBuf};

use crate::sessions::afi_home;
use crate::util;

mod lower;
mod schema;
mod suggest;
mod value;

#[cfg(test)]
mod tests;

/// The file's name inside `$AFI_HOME`.
const FILE_NAME: &str = "config.json";
/// The variable naming one file in place of the default.
const CONFIG_ENV: &str = "AFI_CONFIG";

/// The file a run reads its settings from: the one that was named, or the
/// default when it holds one.
///
/// `named` is `--config`; `AFI_CONFIG` stands in when the flag is absent, and
/// either replaces the default rather than joining it - a caller pointing at one
/// file means that file. A blank value names nothing and leaves the default
/// alone, which is what an exported-but-unset shell variable looks like.
///
/// A named path is returned without checking that it exists, because a path
/// someone typed and that holds no file is a mistake and starting anyway would
/// run with settings nobody chose. The default is returned only when it holds a
/// file, which is what leaves "no config file" as an ordinary run configured by
/// environment and flags.
///
/// `env` supplies `AFI_HOME`, so it wants the env map after any env file has been
/// merged - see `Runtime::resolve_env`.
#[must_use]
pub fn config_path<S: BuildHasher>(
    named: Option<&str>,
    env: &HashMap<String, String, S>,
) -> Option<PathBuf> {
    if let Some(path) = util::nonblank(named.or_else(|| env.get(CONFIG_ENV).map(String::as_str))) {
        return Some(PathBuf::from(path));
    }
    let user = afi_home(env).join(FILE_NAME);
    user.is_file().then_some(user)
}

/// What a run's config files say, lowered to the variable names every setting
/// already travels as.
#[derive(Debug, Clone, Default)]
pub struct FileSettings {
    values: HashMap<String, String>,
    refusals: Vec<String>,
}

impl FileSettings {
    /// Read `file`, or nothing when there is none to read.
    #[must_use]
    pub fn load(file: Option<&Path>) -> Self {
        let mut settings = Self::default();
        if let Some(path) = file {
            settings.read(path);
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
    fn read(&mut self, path: &Path) {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => {
                self.refusals.push(format!("{}: {error}", path.display()));
                return;
            }
        };
        let lowered = lower::read(path, &text);
        self.refusals.extend(lowered.refusals);
        self.values.extend(lowered.pairs);
    }
}
