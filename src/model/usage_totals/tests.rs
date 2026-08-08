use super::*;
use crate::pricing::provider::Provider;

fn usage(input: u64, output: u64, cache: u64, reasoning: u64) -> NormalizedUsage {
    NormalizedUsage {
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: cache,
        cache_write_tokens: 0,
        reasoning_tokens: reasoning,
        estimated: false,
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
            estimated_tokens: 0,
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
        estimated: false,
    });
    totals.add(&NormalizedUsage {
        input_tokens: 50,
        output_tokens: 17,
        cache_read_tokens: 2279,
        cache_write_tokens: 900,
        reasoning_tokens: 0,
        estimated: false,
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
        estimated: false,
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
        estimated: false,
    };
    totals.add(&nonsense);
    totals.add(&usage(10, 10, 10, 10));
    assert_eq!(totals.input_tokens, u64::MAX);
    assert_eq!(totals.cache_write_tokens, u64::MAX);
    assert_eq!(totals.total_tokens(), u64::MAX);
}

/// One test owns the process-wide accumulator. A second one would interleave
/// with it under the parallel runner, so the phases below are plain calls.
#[test]
fn the_process_accumulator_records_and_resets() {
    reset();
    records_one_model();
    reset_clears_every_ledger();
    keeps_models_apart_while_still_totalling();
    reset();
    names_every_source_that_was_billed();
    reset();
}

fn records_one_model() {
    assert!(snapshot().is_empty());
    record(
        "anthropic",
        Some(Provider::Anthropic),
        "claude-sonnet-5",
        &NormalizedUsage {
            input_tokens: 7,
            output_tokens: 8,
            cache_read_tokens: 9,
            cache_write_tokens: 5,
            reasoning_tokens: 1,
            estimated: false,
        },
    );
    let snap = snapshot();
    assert_eq!((snap.input_tokens, snap.output_tokens), (7, 8));
    assert_eq!((snap.cache_read_tokens, snap.reasoning_tokens), (9, 1));
    assert_eq!(snap.cache_write_tokens, 5);
    assert_eq!(snap.requests, 1);
    assert_eq!(billed_sources(), vec!["anthropic".to_string()]);
}

fn reset_clears_every_ledger() {
    reset();
    assert!(snapshot().is_empty(), "reset must clear the accumulator");
    assert!(
        billed_sources().is_empty(),
        "reset must clear the billed sources too, or the next run inherits them"
    );
}

/// A piped session can `/source` its way onto a second model, and the two are
/// not billed alike - so the flat counts stay whole while pricing sees the split.
fn keeps_models_apart_while_still_totalling() {
    record("local", None, "cheap", &usage(100, 20, 0, 0));
    record("local", None, "dear", &usage(300, 40, 0, 0));
    record("local", None, "cheap", &usage(50, 10, 0, 0));
    let by_model: Vec<_> = snapshot_billed()
        .iter()
        .map(|(billed, totals)| (billed.model.clone(), totals.input_tokens, totals.requests))
        .collect();
    assert_eq!(
        by_model,
        vec![("cheap".to_string(), 150, 2), ("dear".to_string(), 300, 1)],
        "models keep first-seen order and accumulate on their own"
    );
    let snap = snapshot();
    assert_eq!((snap.input_tokens, snap.output_tokens), (450, 70));
    assert_eq!(snap.requests, 3);
    assert_eq!(
        billed_sources(),
        vec!["local".to_string()],
        "three requests on one source is still one credential"
    );
}

/// Which credential paid cannot be read off whichever source ended up active, so
/// the ledger remembers the ones that actually spent, in the order they did.
fn names_every_source_that_was_billed() {
    assert!(
        billed_sources().is_empty(),
        "a run that billed nothing names no source"
    );
    record("local", None, "cheap", &usage(100, 20, 0, 0));
    record(
        "anthropic",
        Some(Provider::Anthropic),
        "claude-sonnet-5",
        &usage(300, 40, 0, 0),
    );
    record("local", None, "cheap", &usage(50, 10, 0, 0));
    assert_eq!(
        billed_sources(),
        vec!["local".to_string(), "anthropic".to_string()],
        "first-seen order, and a source that spent twice is named once"
    );
}

#[test]
fn merging_two_models_totals_adds_every_field() {
    let mut left = UsageTotals::default();
    left.add(&usage(1, 2, 3, 4));
    let mut right = UsageTotals::default();
    right.add(&usage(10, 20, 30, 40));
    left.merge(&right);
    assert_eq!(
        left,
        UsageTotals {
            input_tokens: 11,
            output_tokens: 22,
            cache_read_tokens: 33,
            cache_write_tokens: 0,
            reasoning_tokens: 44,
            requests: 2,
            estimated_tokens: 0,
        }
    );
}

#[test]
fn a_refusal_total_is_its_two_halves_and_nothing_more() {
    // The split is why a total is derived rather than counted alongside the halves:
    // a policy block means the model reached for a tool the caller ruled out, while
    // an unattended run with no --yolo denies every mutating call at the gate, and
    // one figure covering both cannot be alerted on. A separately tracked total is
    // the version that drifts.
    let refused = RefusedToolCalls {
        by_policy: 2,
        by_approval: 1,
    };
    assert_eq!(refused.total(), 3);
    assert!(!refused.is_empty());
    // Nothing refused is empty, which is what keeps `usage` null for a run that
    // never started - see `RunSummary::refused`.
    assert!(RefusedToolCalls::default().is_empty());
}

#[test]
fn an_estimate_is_counted_apart_from_the_totals_it_is_part_of() {
    // `estimated_tokens` is a marker over the five classes rather than a sixth
    // one, so it says how much of the run afi counted itself without changing
    // what the run is billed for.
    let mut totals = UsageTotals::default();
    totals.add(&usage(100, 20, 0, 0));
    totals.add(&NormalizedUsage {
        input_tokens: 0,
        output_tokens: 300,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        reasoning_tokens: 0,
        estimated: true,
    });
    assert_eq!(
        totals.total_tokens(),
        420,
        "100 + 20 + 300, and no double count"
    );
    assert_eq!(totals.estimated_tokens, 300, "only the guessed request");
    assert!(totals.has_estimates());
}

#[test]
fn a_run_nobody_guessed_at_carries_no_marker() {
    let mut totals = UsageTotals::default();
    totals.add(&usage(100, 20, 30, 4));
    assert_eq!(totals.estimated_tokens, 0);
    assert!(!totals.has_estimates());
}

#[test]
fn merging_carries_the_marker_with_the_counts() {
    let mut left = UsageTotals::default();
    left.add(&NormalizedUsage {
        input_tokens: 0,
        output_tokens: 10,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        reasoning_tokens: 0,
        estimated: true,
    });
    let mut right = UsageTotals::default();
    right.add(&usage(1, 2, 0, 0));
    left.merge(&right);
    assert_eq!(left.estimated_tokens, 10, "the guess survives the fold");
    assert_eq!(left.total_tokens(), 13);
}
