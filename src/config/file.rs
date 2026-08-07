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

/// Which files a run reads its settings from, lowest precedence first.
///
/// A list rather than one path because the layer is built to merge several and
/// only the search is restricted to one; a caller that has its own idea of where
/// settings live - a test, or a future gated per-project file - hands over as
/// many as it means.
///
/// Every path here has to be readable. [`Self::discover`] lists the default
/// location only when it holds a file, so a missing one never reaches this -
/// which is what leaves "no config file" as an ordinary run configured by
/// environment and flags, while a path someone typed and that holds no file
/// refuses to start.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigFiles {
    /// The paths, lowest precedence first.
    pub paths: Vec<PathBuf>,
}

impl ConfigFiles {
    /// Find the file to read: the one that was named, or the default when it
    /// exists.
    ///
    /// `named` is `--config`; `AFI_CONFIG` stands in when the flag is absent, and
    /// either replaces the default rather than joining it - a caller pointing at
    /// one file means that file. A blank value names nothing and leaves the
    /// default alone, which is what an exported-but-unset shell variable looks
    /// like.
    ///
    /// `env` supplies `AFI_HOME`, so it wants the env map after any env file has
    /// been merged - see `Runtime::resolve_env`.
    #[must_use]
    pub fn discover<S: BuildHasher>(named: Option<&str>, env: &HashMap<String, String, S>) -> Self {
        let named = named
            .or_else(|| env.get(CONFIG_ENV).map(String::as_str))
            .map(str::trim)
            .filter(|path| !path.is_empty());
        if let Some(path) = named {
            // Unchecked on purpose: a path someone typed and that holds no file
            // is a mistake, and starting anyway would run with settings nobody
            // chose.
            return Self {
                paths: vec![PathBuf::from(path)],
            };
        }
        let user = afi_home(env).join(FILE_NAME);
        let paths = if user.is_file() { vec![user] } else { vec![] };
        Self { paths }
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
    /// by file, so a caller that hands over two files keeps the rest of the first
    /// one standing. `discover` only ever finds one.
    #[must_use]
    pub fn load(files: &ConfigFiles) -> Self {
        let mut settings = Self::default();
        for path in &files.paths {
            settings.read(path);
        }
        settings
    }

    /// Why the run must not start: a file that would not read, a key nothing
    /// reads, a value of the wrong shape. Empty when every file read cleanly.
    #[must_use]
    pub fn refusals(&self) -> &[String] {
        &self.refusals
    }

    /// Fill gaps in `env`, leaving anything already set alone - which is what
    /// makes a variable, and the flags written into the map before this, beat the
    /// file.
    ///
    /// A variable that is set to nothing is a gap. `export AFI_X="$UNSET"` is how
    /// a blank arrives, and almost every reader already discards one - approval,
    /// effort, the summary path and the tool lists all treat it as unset - so
    /// letting it shadow the file would land on the built-in default rather than
    /// on either of the two things that were written. For the tool policy that
    /// default is every tool, which is a silent widening of the one setting that
    /// must never widen quietly.
    ///
    /// A file with anything wrong in it applies nothing at all. The run is about
    /// to refuse to start, and a half-applied file is a worse thing to hand to a
    /// caller that ignores the refusal than an unapplied one.
    pub fn apply_to<S: BuildHasher>(&self, env: &mut HashMap<String, String, S>) {
        if !self.refusals.is_empty() {
            return;
        }
        for (name, value) in &self.values {
            match env.get(name) {
                Some(set) if !set.trim().is_empty() => {}
                _ => {
                    env.insert(name.clone(), value.clone());
                }
            }
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
