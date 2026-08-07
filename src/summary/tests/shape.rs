//! Shared scaffolding for the assertions that pin the published shape.
//!
//! Three of them exist - the top-level object, `usage`, and the `auth` block -
//! and each one is the only thing standing between a renamed key and a consumer
//! that silently stops finding it. One helper so the fourth calls this rather
//! than copying it again.

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
