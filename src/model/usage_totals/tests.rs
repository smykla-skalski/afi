use super::*;

fn usage(input: u64, output: u64, cache: u64, reasoning: u64) -> NormalizedUsage {
    NormalizedUsage {
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: cache,
        cache_write_tokens: 0,
        reasoning_tokens: reasoning,
    }
}

#[test]
fn requests_accumulate_rather_than_overwrite() {
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
            cache_write_tokens: 0,
            reasoning_tokens: 0,
            requests: 3,
        }
    );
}

#[test]
fn cache_writes_accumulate_on_their_own_rather_than_into_input() {
    // The case a long session hits repeatedly: the 5-minute TTL lapses, the
    // prefix is rebuilt, and that rebuild is billed above plain input.
    let mut totals = UsageTotals::default();
    totals.add(&NormalizedUsage {
        input_tokens: 100,
        output_tokens: 42,
        cache_read_tokens: 0,
        cache_write_tokens: 2279,
        reasoning_tokens: 0,
    });
    totals.add(&NormalizedUsage {
        input_tokens: 50,
        output_tokens: 17,
        cache_read_tokens: 2279,
        cache_write_tokens: 900,
        reasoning_tokens: 0,
    });
    assert_eq!(totals.cache_write_tokens, 3179);
    assert_eq!(totals.input_tokens, 150, "writes must stay out of input");
    assert_eq!(totals.cache_read_tokens, 2279);
}

#[test]
fn total_is_the_sum_of_five_disjoint_fields() {
    let mut totals = UsageTotals::default();
    totals.add(&NormalizedUsage {
        input_tokens: 100,
        output_tokens: 20,
        cache_read_tokens: 300,
        cache_write_tokens: 50,
        reasoning_tokens: 4,
    });
    assert_eq!(totals.total_tokens(), 474);
}

#[test]
fn no_requests_is_distinguishable_from_zero_tokens() {
    let mut totals = UsageTotals::default();
    assert!(totals.is_empty(), "nothing recorded yet");
    totals.add(&usage(0, 0, 0, 0));
    assert!(
        !totals.is_empty(),
        "a request that reported all zeros still happened"
    );
    assert_eq!(totals.requests, 1);
}

#[test]
fn saturates_instead_of_overflowing() {
    // A provider returning nonsense must not panic a release build or wrap to a
    // small number in a debug one.
    let mut totals = UsageTotals::default();
    let nonsense = NormalizedUsage {
        input_tokens: u64::MAX,
        output_tokens: u64::MAX,
        cache_read_tokens: u64::MAX,
        cache_write_tokens: u64::MAX,
        reasoning_tokens: u64::MAX,
    };
    totals.add(&nonsense);
    totals.add(&usage(10, 10, 10, 10));
    assert_eq!(totals.input_tokens, u64::MAX);
    assert_eq!(totals.cache_write_tokens, u64::MAX);
    assert_eq!(totals.total_tokens(), u64::MAX);
}

#[test]
fn the_process_accumulator_records_and_resets() {
    // Serialized against the other global-state test by the shared mutex; both
    // reset first so ordering between them cannot matter.
    reset();
    assert!(snapshot().is_empty());
    record(&NormalizedUsage {
        input_tokens: 7,
        output_tokens: 8,
        cache_read_tokens: 9,
        cache_write_tokens: 5,
        reasoning_tokens: 1,
    });
    let snap = snapshot();
    assert_eq!((snap.input_tokens, snap.output_tokens), (7, 8));
    assert_eq!((snap.cache_read_tokens, snap.reasoning_tokens), (9, 1));
    assert_eq!(snap.cache_write_tokens, 5);
    assert_eq!(snap.requests, 1);
    reset();
    assert!(snapshot().is_empty(), "reset must clear the accumulator");
}
