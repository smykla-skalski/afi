//! What a spend cap asks of the rate table, as opposed to what a summary asks.
//!
//! Split from the rest because the question is different: `run_cost_usd` reports
//! a figure or reports nothing, while `run_cost` has to answer a cap that cannot
//! act on "nothing". The two diverge only where a class was spent on and has no
//! rate, which is exactly what these pin.

use super::{SONNET, run, totals};
use crate::pricing::{Priced, Pricing};

#[test]
fn a_model_with_no_rates_at_all_is_the_most_unpriceable_kind() {
    // The trap this guards: `rates_for` returns `None` for a model nothing
    // prices, and `unpriceable` returns `None` for a model it *can* price. Wiring
    // the first straight through to the second with `?` inverts the answer and
    // lets a budgeted run start with a cap that can never fire.
    let pricing = Pricing::parse(Some(SONNET)).unwrap();
    assert!(
        pricing
            .unpriceable(None, "a-model-nothing-prices")
            .is_some_and(|why| why.contains("no rate for model")),
        "a model with no rates must report why, not report itself as fine"
    );
    assert_eq!(
        pricing.unpriceable(None, "claude-sonnet-5"),
        None,
        "and a fully priced model must report nothing"
    );
}

#[test]
fn a_model_priced_for_only_half_the_run_says_which_half() {
    let input_only = Pricing::parse(Some(r#"{"m": {"input": 3}}"#)).unwrap();
    assert!(
        input_only
            .unpriceable(None, "m")
            .is_some_and(|why| why.contains("\"output\"")),
        "every request spends on output, so a missing output rate cannot be capped around"
    );
    let output_only = Pricing::parse(Some(r#"{"m": {"output": 3}}"#)).unwrap();
    assert!(
        output_only
            .unpriceable(None, "m")
            .is_some_and(|why| why.contains("\"input\""))
    );
}

#[test]
fn a_cap_survives_a_cache_read_the_table_has_no_rate_for() {
    // The failure this pair exists to prevent: `unpriceable` demands only input
    // and output, so a run starts - and then an OpenAI-compatible endpoint with
    // prefix caching reports `cached_tokens` from the second request on, and a
    // strict price kills the run mid-turn with the very refusal the pre-flight
    // check promised to make before spending anything.
    let pricing = Pricing::parse(Some(r#"{"m": {"input": 10, "output": 10}}"#)).unwrap();
    assert_eq!(pricing.unpriceable(None, "m"), None, "the run may start");

    let mut usage = totals(0, 0, 1_000_000, 0);
    usage.input_tokens = 0;
    let billed = run("m", usage);

    // The cap prices the cache read at the model's own input rate - its ceiling,
    // since a provider charges less for a cached prompt token, never more.
    assert_eq!(
        pricing.run_cost(&billed),
        Priced::Spent(10_000_000),
        "a cap must get a number it can act on"
    );
    // The summary still refuses to guess: an over-estimate reported as the bill
    // is the wrong number stated confidently.
    assert_eq!(
        pricing.run_cost_usd(&billed),
        None,
        "cost_usd reports a figure or reports nothing"
    );
}

#[test]
fn a_cache_write_has_no_ceiling_to_fall_back_to() {
    // The one class that can cost more than input - Anthropic bills a write
    // above it - so there is no safe substitute and a cap must say so rather
    // than under-count.
    let pricing = Pricing::parse(Some(r#"{"m": {"input": 10, "output": 10}}"#)).unwrap();
    let billed = run("m", totals(0, 0, 0, 1_000_000));
    assert!(matches!(pricing.run_cost(&billed), Priced::Unpriceable(_)));
}

#[test]
fn a_priced_cache_read_is_billed_at_its_own_rate_not_the_ceiling() {
    // The fallback must not quietly over-bill a model that *does* carry a rate.
    let pricing = Pricing::parse(Some(
        r#"{"m": {"input": 10, "output": 10, "cache_read": 1}}"#,
    ))
    .unwrap();
    let billed = run("m", totals(0, 0, 1_000_000, 0));
    assert_eq!(pricing.run_cost(&billed), Priced::Spent(1_000_000));
    assert_eq!(pricing.run_cost_usd(&billed), Some(1.0));
}
