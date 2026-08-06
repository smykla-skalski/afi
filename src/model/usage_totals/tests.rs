use super::*;

fn usage(input: u64, output: u64, cache: u64, reasoning: u64) -> NormalizedUsage {
    NormalizedUsage {
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: cache,
        reasoning_tokens: reasoning,
    }
}

#[test]
fn turns_accumulate_rather_than_overwrite() {
    // The bug this exists to prevent: a per-turn value replacing the run total,
    // which under-reports every run after the first turn.
    let mut totals = UsageTotals::default();
    totals.add(&usage(123, 120, 2279, 0));
    totals.add(&usage(924, 216, 2279, 0));
    totals.add(&usage(3038, 173, 2279, 0));
    assert_eq!(
        totals,
        UsageTotals {
            input_tokens: 4085,
            output_tokens: 509,
            cache_read_tokens: 6837,
            reasoning_tokens: 0,
            turns: 3,
        }
    );
}

#[test]
fn total_is_the_sum_of_four_disjoint_fields() {
    let mut totals = UsageTotals::default();
    totals.add(&usage(100, 20, 300, 4));
    assert_eq!(totals.total_tokens(), 424);
}

#[test]
fn no_turns_is_distinguishable_from_zero_tokens() {
    let mut totals = UsageTotals::default();
    assert!(totals.is_empty(), "nothing recorded yet");
    totals.add(&usage(0, 0, 0, 0));
    assert!(
        !totals.is_empty(),
        "a turn that reported all zeros still happened"
    );
    assert_eq!(totals.turns, 1);
}

#[test]
fn saturates_instead_of_overflowing() {
    // A provider returning nonsense must not panic a release build or wrap to a
    // small number in a debug one.
    let mut totals = UsageTotals::default();
    totals.add(&usage(u64::MAX, u64::MAX, u64::MAX, u64::MAX));
    totals.add(&usage(10, 10, 10, 10));
    assert_eq!(totals.input_tokens, u64::MAX);
    assert_eq!(totals.total_tokens(), u64::MAX);
}

#[test]
fn the_process_accumulator_records_and_resets() {
    // Serialized against the other global-state test by the shared mutex; both
    // reset first so ordering between them cannot matter.
    reset();
    assert!(snapshot().is_empty());
    record(&usage(7, 8, 9, 1));
    let snap = snapshot();
    assert_eq!((snap.input_tokens, snap.output_tokens), (7, 8));
    assert_eq!((snap.cache_read_tokens, snap.reasoning_tokens), (9, 1));
    assert_eq!(snap.turns, 1);
    reset();
    assert!(snapshot().is_empty(), "reset must clear the accumulator");
}
