//! Shared scaffolding for the assertions that pin the published shape.
//!
//! Four of them exist - the top-level object, `usage`, the `auth` block, and an
//! entry of the per-source breakdown - and each one is the only thing standing
//! between a renamed key and a consumer that silently stops finding it. One
//! helper so the fifth calls this rather than copying it again.

use serde_json::Value;

/// An object's keys, sorted, so an assertion pins the set rather than whatever
/// order the map happens to hand them back in.
pub(in crate::summary) fn sorted_keys(value: &Value) -> Vec<&str> {
    let mut names: Vec<&str> = value
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    names.sort_unstable();
    names
}
