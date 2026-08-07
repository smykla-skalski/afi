//! One config file into the environment pairs its settings already travel as.
//!
//! Nothing here resolves a setting. It reads the file, refuses what it cannot
//! account for, and hands back `(variable, value)` pairs for the layer above to
//! fill gaps in the env map with - so a file-set value reaches its reader
//! through exactly the code an exported variable reaches it through, and the two
//! cannot disagree about what the setting means.
//!
//! Every refusal names the file and the path to the key, because a config error
//! that says only what is wrong leaves the operator grepping their own files for
//! which one said it.

use std::path::Path;

use serde_json::{Map, Value};

use super::super::sources;
use super::Origin;
use super::schema::{self, Merge, Scope};
use super::suggest::nearest;
use super::value::{self, Convert};

/// One file being read: what it said, and why the run must not start.
pub(super) struct Lowered<'f> {
    file: &'f Path,
    /// Where this file came from, which decides what it is allowed to set - see
    /// [`Scope`].
    origin: Origin,
    /// The variables this file sets, each with how it combines with what an
    /// earlier file said - see [`Merge`].
    pub pairs: Vec<(String, String, Merge)>,
    pub refusals: Vec<String>,
}

/// Read one file's text into environment pairs.
///
/// A file that is entirely blank sets nothing and is not an error: it is what a
/// `touch` leaves behind, and refusing it would answer "I have not written this
/// yet" with a failed run. Anything else has to parse.
pub(super) fn read<'f>(file: &'f Path, text: &str, origin: Origin) -> Lowered<'f> {
    let mut out = Lowered {
        file,
        origin,
        pairs: Vec::new(),
        refusals: Vec::new(),
    };
    if text.trim().is_empty() {
        return out;
    }
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(root)) => out.root(&root),
        Ok(other) => {
            let message = value::expected("a JSON object", &other);
            out.refuse("the file", &message);
        }
        Err(error) => out.bad_file(&format!("is not readable JSON ({error})")),
    }
    out
}

impl Lowered<'_> {
    /// Walk the root object. Every key is either a block that carries structure
    /// or one setting.
    fn root(&mut self, root: &Map<String, Value>) {
        for (key, value) in root {
            match key.as_str() {
                "sources" => self.sources(value),
                "prices" => self.prices(value),
                "anthropic" => self.anthropic(value),
                _ => self.field("", schema::TOP, &schema::BLOCKS, key, value),
            }
        }
    }

    /// The `sources` block: one object per source, keyed by its name.
    fn sources(&mut self, value: &Value) {
        let Some(block) = self.object("sources", value) else {
            return;
        };
        for (name, fields) in block {
            self.source(name, fields);
        }
    }

    /// One named source.
    fn source(&mut self, name: &str, value: &Value) {
        let path = format!("sources.{name}");
        let Some(prefix) = self.source_prefix(&path, name) else {
            return;
        };
        let Some(fields) = self.object(&path, value) else {
            return;
        };
        for (key, field) in fields {
            self.field_as(&path, &prefix, schema::SOURCE, &[], key, field);
        }
    }

    /// The `AFI_SOURCE_<NAME>_` prefix this source's fields lower to, or `None`
    /// when the name cannot carry one.
    ///
    /// Lowercase only, which is stricter than it looks necessary. The name has to
    /// survive becoming part of a variable name, which is uppercased on the way
    /// in and lowercased on the way back out, so a name with a capital in it
    /// comes back as a different name: a source written `Zai` registers as `zai`,
    /// and `"active": "Zai"` then matches nothing while naming the source that is
    /// sitting right there. Refusing beats renaming in silence.
    fn source_prefix(&mut self, path: &str, name: &str) -> Option<String> {
        if name.is_empty() || !name.chars().all(usable_in_name) {
            self.refuse(
                path,
                "is not a usable source name (lowercase letters, digits, '-', \
                 and '_' only, since the name has to survive becoming part of a \
                 variable name)",
            );
            return None;
        }
        Some(sources::source_prefix(name))
    }

    /// The `prices` table, checked entry by entry and then written back whole.
    ///
    /// Only the shape is checked here - an object of numbers under known class
    /// names. What the numbers may be (nothing negative, nothing finer than the
    /// sixth decimal place, no model named twice) belongs to `Pricing`, which
    /// already reports it at startup and disables cost reporting for the run.
    fn prices(&mut self, value: &Value) {
        let Some(table) = self.object("prices", value) else {
            return;
        };
        for (model, rates) in table {
            self.rates(model, rates);
        }
        // Per model, so a project file pricing one does not drop the rates for
        // every other one.
        self.set("prices", "AFI_PRICES", value::object, value, Merge::Object);
    }

    /// One model's rates.
    fn rates(&mut self, model: &str, value: &Value) {
        let path = format!("prices.{model}");
        let Some(rates) = self.object(&path, value) else {
            return;
        };
        for (class, rate) in rates {
            if !schema::PRICE_CLASSES.contains(&class.as_str()) {
                self.unknown(&path, class, &schema::PRICE_CLASSES);
            } else if !rate.is_number() {
                let message = value::expected("a number", rate);
                self.refuse(&format!("{path}.{class}"), &message);
            }
        }
    }

    /// The `anthropic` block: the built-in source's overrides, plus federation.
    fn anthropic(&mut self, value: &Value) {
        let Some(block) = self.object("anthropic", value) else {
            return;
        };
        for (key, field) in block {
            if key == "federation" {
                self.federation(field);
            } else {
                self.field("anthropic", schema::ANTHROPIC, &["federation"], key, field);
            }
        }
    }

    /// The `anthropic.federation` block.
    fn federation(&mut self, value: &Value) {
        let path = "anthropic.federation";
        let Some(block) = self.object(path, value) else {
            return;
        };
        for (key, field) in block {
            self.field(path, schema::FEDERATION, &[], key, field);
        }
    }

    /// One field of a block whose variables carry their full names.
    ///
    /// `nested` names the blocks that sit beside `table`, so a misspelling of one
    /// is suggested as readily as a misspelled setting. The caller knows them; a
    /// lookup keyed on the prefix would be this function guessing at what it was
    /// already told.
    fn field(
        &mut self,
        prefix: &str,
        table: &'static [schema::Setting],
        nested: &[&'static str],
        key: &str,
        value: &Value,
    ) {
        self.field_as(prefix, "", table, nested, key, value);
    }

    /// [`Self::field`], with `env_prefix` in front of the variable it lowers to -
    /// which is how a source's fields reach `AFI_SOURCE_<NAME>_*`.
    fn field_as(
        &mut self,
        prefix: &str,
        env_prefix: &str,
        table: &'static [schema::Setting],
        nested: &[&'static str],
        key: &str,
        value: &Value,
    ) {
        let path = key_path(prefix, key);
        if let Some((_, why)) = schema::REFUSED.iter().find(|(name, _)| *name == key) {
            self.refuse(&path, why);
            return;
        }
        let Some(setting) = schema::find(table, key) else {
            self.unknown(prefix, key, &schema::keys(table, nested));
            return;
        };
        if self.origin == Origin::WorkingTree && setting.scope == Scope::Operator {
            self.refuse(&path, PROJECT_REFUSAL);
            return;
        }
        let env = format!("{env_prefix}{}", setting.env);
        self.set(&path, &env, setting.convert, value, setting.merge);
    }

    /// Record one variable, or why the value could not become one.
    fn set(&mut self, path: &str, env: &str, convert: Convert, value: &Value, merge: Merge) {
        match convert(value) {
            Ok(text) => self.pairs.push((env.to_string(), text, merge)),
            Err(why) => self.refuse(path, &why),
        }
    }

    /// Borrow `value` as an object, refusing anything else.
    fn object<'v>(&mut self, path: &str, value: &'v Value) -> Option<&'v Map<String, Value>> {
        if let Some(map) = value.as_object() {
            return Some(map);
        }
        let message = value::expected("a JSON object", value);
        self.refuse(path, &message);
        None
    }

    /// `<file>: <path> <message>`.
    fn refuse(&mut self, path: &str, message: &str) {
        let file = self.file.display();
        self.refusals.push(format!("{file}: {path} {message}"));
    }

    /// `<file>: <message>`, for what is wrong with the file rather than a key.
    fn bad_file(&mut self, message: &str) {
        let file = self.file.display();
        self.refusals.push(format!("{file}: {message}"));
    }

    /// A key nothing reads, with the nearest one that is read.
    fn unknown(&mut self, prefix: &str, key: &str, candidates: &[&str]) {
        let path = key_path(prefix, key);
        let hint = nearest(key, candidates)
            .map_or_else(String::new, |near| format!(" (did you mean \"{near}\"?)"));
        self.bad_file(&format!("unknown key \"{path}\"{hint}"));
    }
}

/// What a project file is told when it reaches for a key only the operator's own
/// file may set.
const PROJECT_REFUSAL: &str = "cannot be set by a file in the working directory - \
     a repository does not choose where requests go, whose instructions the model \
     follows, or whether you are asked before a tool runs; move it to \
     $AFI_HOME/config.json, or name this file with --config";

/// `prefix.key`, or bare `key` at the root, which has no prefix to join.
fn key_path(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_string()
    } else {
        format!("{prefix}.{key}")
    }
}

/// Characters a source name may use, being the characters that survive a round
/// trip through a variable name unchanged.
fn usable_in_name(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-'
}
