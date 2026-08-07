//! The "did you mean" itself.

use super::super::suggest::nearest;

const KEYS: [&str; 4] = ["active", "approval", "max_tokens", "sources"];

#[test]
fn a_typo_gets_its_key() {
    assert_eq!(nearest("activ", &KEYS), Some("active"));
    assert_eq!(nearest("aproval", &KEYS), Some("approval"));
    assert_eq!(nearest("max_token", &KEYS), Some("max_tokens"));
}

#[test]
fn something_unrelated_gets_nothing() {
    assert_eq!(nearest("telemetry", &KEYS), None);
    assert_eq!(nearest("x", &KEYS), None);
}

#[test]
fn an_abbreviation_gets_the_whole_key() {
    // Three edits by the letter, one mistake by eye.
    assert_eq!(nearest("rule", &["rule_id"]), Some("rule_id"));
    assert_eq!(nearest("api", &["api_key", "app_name"]), Some("api_key"));
    // Two characters claim nothing, or every key ending in an id would answer.
    assert_eq!(nearest("id", &["rule_id", "organization_id"]), None);
}

#[test]
fn a_longer_key_tolerates_more() {
    // The threshold grows with the key, so two characters wrong in the middle of
    // a long name is still a typo rather than a different word.
    assert_eq!(
        nearest("recovery_dry_multiplyer", &["recovery_dry_multiplier"]),
        Some("recovery_dry_multiplier")
    );
}

#[test]
fn the_same_typo_always_gets_the_same_answer() {
    // Ties go to the alphabetically first candidate rather than to whichever the
    // table happens to list first.
    let candidates = ["output", "input"];
    assert_eq!(nearest("nput", &candidates), Some("input"));
    let ambiguous = ["bb", "ab"];
    assert_eq!(nearest("cb", &ambiguous), Some("ab"));
    assert_eq!(nearest("cb", &["ab", "bb"]), Some("ab"));
}
