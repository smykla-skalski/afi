//! That the table still covers every setting there is.
//!
//! The layer's contract is "every `AFI_*` run setting has a key". Held by memory,
//! that lasts until the next feature adds a variable and stops at the flag - which
//! is what happened while this branch was open, twice. So it is checked instead,
//! by reading the source for the variable names it mentions and subtracting the
//! ones the schema accounts for. A new variable with no row fails here and names
//! itself.
//!
//! Possible only because every read in this crate is a string literal against the
//! env map. The one exception is the `AFI_SOURCE_<NAME>_*` family, whose names are
//! built from [`schema::SOURCE`]'s own suffixes, so it is expanded rather than
//! matched.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use regex::Regex;

use super::super::schema;

/// Variables that exist and deliberately have no key. Every entry needs a reason
/// here and a matching sentence in `schema`'s module doc.
const EXEMPT: [&str; 9] = [
    // Read before the config file is located, so a key naming it could not take
    // effect.
    "AFI_ENV_FILE",
    // The flat spelling of one source. `sources` is the structured one.
    "AFI_BASE_URL",
    "AFI_MODEL",
    "AFI_API_KEY",
    // Stamped in by the build, not by whoever runs it.
    "AFI_BUILD_COMMIT",
    "AFI_BUILD_COMMIT_DATE",
    "AFI_BUILD_DIRTY",
    "AFI_BUILD_RUSTC",
    "AFI_BUILD_TARGET",
];

#[test]
fn every_afi_variable_the_source_reads_has_a_key_or_a_reason() {
    let mut missing: Vec<String> = mentioned()
        .into_iter()
        .filter(|name| !accounted_for(name))
        .collect();
    missing.sort();
    assert!(
        missing.is_empty(),
        "these variables have no key in `schema` and no entry in `EXEMPT`: {}\n\
         Add a row, or exempt it and say why in the module doc.",
        missing.join(", ")
    );
}

#[test]
fn the_exemptions_are_all_still_real_variables() {
    // An exemption for a variable that no longer exists is a reason nobody can
    // check, and it hides the next one that needs looking at.
    let mentioned = mentioned();
    let stale: Vec<&str> = EXEMPT
        .into_iter()
        .filter(|name| !mentioned.contains(*name))
        .collect();
    assert!(
        stale.is_empty(),
        "these exemptions name variables the source no longer reads: {}",
        stale.join(", ")
    );
}

/// Every `AFI_*` name any source file mentions.
fn mentioned() -> BTreeSet<String> {
    // A literal ending in `_` is a prefix the code builds a name from, not a name
    // - `AFI_SOURCE_` is stripped rather than read.
    let name = Regex::new(r#""(AFI_[A-Z0-9_]*[A-Z0-9])""#).unwrap();
    let mut found = BTreeSet::new();
    walk(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src").as_path(),
        &mut |text| {
            for hit in name.captures_iter(text) {
                found.insert(hit[1].to_string());
            }
        },
    );
    assert!(
        found.len() > 30,
        "only {} variables found - the walk is not reading the source",
        found.len()
    );
    found
}

/// Whether the schema carries `name`, one way or another.
fn accounted_for(name: &str) -> bool {
    if EXEMPT.contains(&name) {
        return true;
    }
    // A name a block carries rather than a row: `source_order` writes
    // `AFI_SOURCES`, `prices` writes `AFI_PRICES`, and `AFI_CONFIG` names the
    // file itself rather than anything in it.
    if matches!(name, "AFI_SOURCES" | "AFI_PRICES" | "AFI_CONFIG") {
        return true;
    }
    if schema::TOP.iter().any(|s| s.env == name) || schema::ANTHROPIC.iter().any(|s| s.env == name)
    {
        return true;
    }
    // `AFI_SOURCE_<NAME>_<FIELD>`: the name is the operator's, the field is the
    // schema's.
    name.strip_prefix("AFI_SOURCE_").is_some_and(|rest| {
        schema::SOURCE
            .iter()
            .any(|field| rest.ends_with(&format!("_{}", field.env)))
    })
}

/// Hand every `.rs` file under `dir` to `see`.
fn walk(dir: &Path, see: &mut impl FnMut(&str)) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, see);
        } else if path.extension().is_some_and(|ext| ext == "rs")
            && let Ok(text) = fs::read_to_string(&path)
        {
            see(&text);
        }
    }
}
