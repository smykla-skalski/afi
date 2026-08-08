//! The cap's logic, driven against a hand-built ledger.
//!
//! Nothing here owns the process. `Guard::checkpoint` takes the snapshot as an
//! argument precisely so these run in parallel with everything else, which is
//! what the accumulator's own tests cannot do.

use std::collections::HashMap;

use super::{Crossing, Guard, Verdict, checkpoint, install, may_spend, money, outcome, reset};
use crate::config::Budget;
use crate::config::budget::resolve_budget;
use crate::model::stream::NormalizedUsage;
use crate::model::usage_totals::{self, Billed, UsageTotals};
use crate::pricing::Pricing;

/// $1.00 of input per million tokens, so a million tokens is exactly a dollar.
const RATES: &str = r#"{"m": {"input": 1, "output": 1}}"#;

fn budget(usd: &str) -> Budget {
    resolve_budget(Some(usd), &HashMap::new())
        .expect("the fixture must resolve")
        .expect("the fixture must set a budget")
}

fn guard(usd: &str) -> Guard {
    Guard {
        budget: budget(usd),
        pricing: Pricing::parse(Some(RATES)).expect("the rates must parse"),
        converged: false,
        stopped: false,
    }
}

/// The same, but counted by afi rather than reported by the provider.
fn guessed(input: u64) -> Vec<(Billed, UsageTotals)> {
    let mut out = spent(input);
    out[0].1.estimated_tokens = input;
    out
}

/// A ledger holding `input` input tokens on the priced model.
fn spent(input: u64) -> Vec<(Billed, UsageTotals)> {
    vec![(
        Billed {
            source: "local".to_string(),
            provider: None,
            model: "m".to_string(),
        },
        UsageTotals {
            input_tokens: input,
            requests: 1,
            ..UsageTotals::default()
        },
    )]
}

#[test]
fn a_run_under_the_soft_threshold_is_told_nothing() {
    let mut guard = guard("5");
    // $3.99 of a $5 cap: the soft threshold is $4.00.
    assert_eq!(guard.checkpoint(&spent(3_990_000)), Verdict::Under);
    assert!(!guard.converged);
    assert!(!guard.stopped);
}

#[test]
fn a_run_that_has_spent_nothing_yet_is_not_stopped() {
    // Zero, not unknown. The distinction the `Priced` enum exists for: an empty
    // ledger must not read as "cannot price" and end the run before it starts.
    let mut guard = guard("5");
    assert_eq!(guard.checkpoint(&[]), Verdict::Under);
    assert!(!guard.stopped);
}

#[test]
fn the_converge_note_is_offered_once_and_never_again() {
    // "Once per run" is a latch on the guard rather than a property of the
    // message history, which is the difference between mechanical enforcement
    // and hoping a nudge survives the next one.
    let mut guard = guard("5");
    let at = Crossing {
        spent: 4_100_000,
        limit: 5_000_000,
    };
    assert_eq!(guard.checkpoint(&spent(4_100_000)), Verdict::Soft(at));
    assert_eq!(guard.checkpoint(&spent(4_200_000)), Verdict::Under);
    assert_eq!(guard.checkpoint(&spent(4_300_000)), Verdict::Under);
    assert!(guard.converged);
}

#[test]
fn the_hard_threshold_stops_the_run_and_stays_stopped() {
    let mut guard = guard("5");
    let at = Crossing {
        spent: 4_800_000,
        limit: 5_000_000,
    };
    // $4.80 of $5 is past the 0.95 hard threshold at $4.75.
    assert_eq!(guard.checkpoint(&spent(4_800_000)), Verdict::Hard(at));
    assert!(guard.stopped);
}

#[test]
fn a_stopped_run_is_still_stopped_on_the_next_loop() {
    // A piped session starts a fresh `run_model_turn_loop` for every user turn,
    // and the loop acts only on `Hard`. Answering the second one with anything
    // else let the turn after the cap open a request and spend past it.
    let mut guard = guard("5");
    assert!(matches!(
        guard.checkpoint(&spent(9_000_000)),
        Verdict::Hard(_)
    ));
    assert!(
        matches!(guard.checkpoint(&spent(9_000_000)), Verdict::Hard(_)),
        "a stopped run must never read as carry-on"
    );
}

/// The process-wide guard, held under [`run_state_lock`] for the duration.
///
/// `may_spend` is what stops `/compress` issuing a billed request after the cap
/// has fired, and it had no test at all: inverting it left the whole suite
/// green. The phases below are plain calls rather than separate `#[test]`s
/// because they are one sequence against one guard; the lock is what keeps the
/// *other* owners of this state out while it runs.
#[test]
fn the_installed_guard_answers_may_spend() {
    let _run = usage_totals::run_state_lock();
    reset();
    assert!(
        may_spend(),
        "a run with no budget may always spend - which is every run today"
    );

    install(Some(budget("5")), Pricing::parse(Some(RATES)).as_ref());
    assert!(may_spend(), "a budget nothing has spent against yet");

    stops_spending_without_waiting_to_be_told();
    stops_spending_once_the_cap_fires();

    reset();
    usage_totals::reset();
    assert!(may_spend(), "reset clears the latch for the next run");
}

/// Spend past the cap with nobody checkpointing, which is the ordinary shape of
/// a finished turn.
///
/// `checkpoint` is called only at the *top* of a turn-loop iteration, so a turn
/// that ended `TURN_DONE` never reports the spend it just made. A `may_spend`
/// that answered from the `stopped` flag therefore said yes however far over
/// the cap the run was, and `/compress` - the exact case the read-only question
/// exists for - billed on unbounded. Two user turns over budget then let three
/// `/compress` calls through, and the summary reported `stopped: false`.
fn stops_spending_without_waiting_to_be_told() {
    usage_totals::reset();
    usage_totals::record("local", None, "m", &spent_usage(9_000_000));
    assert!(
        !may_spend(),
        "the cap must hold on the ledger, not on whether the loop looked recently"
    );
    assert!(
        outcome().is_some_and(|o| !o.stopped),
        "and it must answer without latching, which only a checkpoint may do"
    );
}

/// Drive the process-wide guard past its hard threshold the way a turn does.
fn stops_spending_once_the_cap_fires() {
    usage_totals::reset();
    usage_totals::record("local", None, "m", &spent_usage(9_000_000));
    assert!(matches!(checkpoint(), Verdict::Hard(_)));
    assert!(
        !may_spend(),
        "/compress must not be able to spend after the cap has fired"
    );
    assert!(outcome().is_some_and(|o| o.stopped));
}

/// One request's worth of reported input tokens.
fn spent_usage(input: u64) -> NormalizedUsage {
    NormalizedUsage {
        input_tokens: input,
        ..NormalizedUsage::default()
    }
}

#[test]
fn one_large_turn_may_pass_the_soft_threshold_without_converging() {
    // The note is best effort and the stop is not. A turn big enough to jump the
    // gap gets no note, and must still stop.
    let mut guard = guard("5");
    assert!(matches!(
        guard.checkpoint(&spent(9_000_000)),
        Verdict::Hard(_)
    ));
    assert!(!guard.converged, "there was never a request to tell");
    assert!(guard.stopped);
}

#[test]
fn spend_afi_had_to_guess_at_cannot_be_capped() {
    // The chars-per-token fallback records no input tokens at all, so a run
    // capped against it would over-run by roughly the whole prompt while
    // reporting a confident figure. Stopping is the only honest answer.
    let mut guard = guard("5");
    let verdict = guard.checkpoint(&guessed(1));
    match verdict {
        Verdict::Unpriceable(why) => {
            assert!(why.contains("counted the tokens itself"), "{why}");
        }
        other => panic!("an estimate must not be capped against: {other:?}"),
    }
    assert!(
        !guard.stopped,
        "the cap did not stop it - the measurement did"
    );
}

#[test]
fn spend_on_a_model_with_no_rates_cannot_be_capped() {
    // The `/source`-switch case, found at the next checkpoint. A budget that
    // cannot be measured must never be treated as no budget.
    let mut guard = guard("5");
    let elsewhere = vec![(
        Billed {
            source: "other".to_string(),
            provider: None,
            model: "unpriced".to_string(),
        },
        UsageTotals {
            input_tokens: 10,
            requests: 1,
            ..UsageTotals::default()
        },
    )];
    match guard.checkpoint(&elsewhere) {
        Verdict::Unpriceable(why) => assert!(why.contains("unpriced"), "{why}"),
        other => panic!("an unpriced model must not be capped against: {other:?}"),
    }
}

#[test]
fn the_thresholds_move_with_the_ratios() {
    let mut env = HashMap::new();
    env.insert("AFI_SOFT_BUDGET_RATIO".to_string(), "0.5".to_string());
    env.insert("AFI_HARD_BUDGET_RATIO".to_string(), "0.6".to_string());
    let mut guard = Guard {
        budget: resolve_budget(Some("10"), &env).unwrap().unwrap(),
        pricing: Pricing::parse(Some(RATES)).expect("the rates must parse"),
        converged: false,
        stopped: false,
    };
    assert_eq!(guard.checkpoint(&spent(4_999_999)), Verdict::Under);
    assert!(matches!(
        guard.checkpoint(&spent(5_000_000)),
        Verdict::Soft(_)
    ));
    assert!(matches!(
        guard.checkpoint(&spent(6_000_000)),
        Verdict::Hard(_)
    ));
}

#[test]
fn money_reads_as_money_at_every_size() {
    // `{:.2}` on a float would print a sub-cent cap as `$0.00`, and a test
    // harness sets one of those.
    assert_eq!(money(5_000_000), "$5.00");
    assert_eq!(money(4_830_000), "$4.83");
    assert_eq!(money(0), "$0.00");
    assert_eq!(money(1_000), "$0.001");
    assert_eq!(money(4_831_402), "$4.831402");
}

#[test]
fn a_crossing_names_both_figures() {
    let at = Crossing {
        spent: 4_020_000,
        limit: 5_000_000,
    };
    assert_eq!(at.describe(), "$4.02 of $5.00");
}

#[test]
fn a_run_that_stops_being_measurable_stops_rather_than_riding_its_last_figure() {
    // The pre-flight checkpoint measures zero before anything has been sent. A
    // guard that had once priced the run cleanly must still refuse it the moment
    // a request arrives that afi had to guess at - "it was measurable a turn ago"
    // is not a cap.
    let mut guard = guard("5");
    assert_eq!(guard.checkpoint(&[]), Verdict::Under);
    assert!(matches!(
        guard.checkpoint(&guessed(1)),
        Verdict::Unpriceable(_)
    ));
}
