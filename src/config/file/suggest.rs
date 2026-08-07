//! The "did you mean" on an unknown key.
//!
//! A key nothing knows is refused rather than ignored, which means the message
//! is the whole of the help the operator gets. Listing every valid key answers a
//! typo with a wall of text; naming the one key a character away answers it in
//! four words.

use std::mem;

/// The candidate closest to `key`, when one is close enough to be worth naming.
///
/// The threshold grows with the key's length so `activ` still suggests `active`
/// while `x` suggests nothing.
///
/// A tie goes to the candidate nearest in length - `source` means `sources`
/// rather than `source_order` - and then to the alphabetically first, so the same
/// typo always gets the same answer whatever order the table lists keys in.
pub(super) fn nearest<'c>(key: &str, candidates: &[&'c str]) -> Option<&'c str> {
    // Characters throughout, never bytes: the distance below counts characters,
    // and a threshold or a tie-break measured in bytes would disagree with it the
    // moment a key holds anything outside ASCII.
    let written = key.chars().count();
    let limit = 2.max(written / 4);
    candidates
        .iter()
        .copied()
        .map(|candidate| (score(key, candidate), candidate))
        .filter(|(d, _)| *d <= limit)
        .min_by_key(|(d, candidate)| (*d, candidate.chars().count().abs_diff(written), *candidate))
        .map(|(_, candidate)| candidate)
}

/// Distance, counting an abbreviation as one edit however much of the key is
/// missing.
///
/// `rule` for `rule_id` is three edits away and is obviously the same key
/// shortened; measuring it honestly would answer it with nothing. Three
/// characters of shared start is the floor, so `id` does not claim every key
/// ending in one.
fn score(key: &str, candidate: &str) -> usize {
    let abbreviated = key.starts_with(candidate) || candidate.starts_with(key);
    if key.chars().count() >= 3 && abbreviated {
        return 1;
    }
    distance(key, candidate)
}

/// Levenshtein distance, counting characters rather than bytes so a key with an
/// accidental multi-byte character is measured rather than mismeasured.
fn distance(a: &str, b: &str) -> usize {
    let right: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=right.len()).collect();
    let mut cur = vec![0; right.len() + 1];
    for (i, from) in a.chars().enumerate() {
        cur[0] = i + 1;
        for (j, to) in right.iter().enumerate() {
            let substitute = prev[j] + usize::from(from != *to);
            cur[j + 1] = substitute.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        mem::swap(&mut prev, &mut cur);
    }
    prev[right.len()]
}
