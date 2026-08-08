use super::*;
use crate::pricing::provider::Provider;

/// Anthropic's published Sonnet rates, the table a CI job would actually write.
const SONNET: &str = r#"{
  "claude-sonnet-5": {
    "input": 3,
    "output": 15,
    "cache_read": 0.3,
    "cache_write": 3.75
  }
}"#;

fn totals(input: u64, output: u64, cache_read: u64, cache_write: u64) -> UsageTotals {
    UsageTotals {
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: cache_read,
        cache_write_tokens: cache_write,
        reasoning_tokens: 0,
        requests: 1,
        estimated_tokens: 0,
    }
}

/// One entry in the ledger, on an endpoint afi carries no rates for, so what is
/// under test is the caller's own table and nothing beneath it.
fn run(model: &str, usage: UsageTotals) -> Vec<(Billed, UsageTotals)> {
    vec![(billed(model), usage)]
}

fn billed(model: &str) -> Billed {
    Billed {
        source: "local".to_string(),
        provider: None,
        model: model.to_string(),
    }
}

// --- rate parsing ---------------------------------------------------------------

#[test]
fn a_rate_is_read_as_an_exact_decimal() {
    assert_eq!(millionths("3"), Some(3_000_000));
    assert_eq!(millionths("0.3"), Some(300_000));
    assert_eq!(millionths("3.75"), Some(3_750_000));
    assert_eq!(millionths(".5"), Some(500_000));
    assert_eq!(millionths("0.000001"), Some(1));
    assert_eq!(millionths("0"), Some(0));
}

#[test]
fn an_exponent_is_the_number_it_denotes_rather_than_a_rejection() {
    // These reach the parser whenever serde_json renders a rate that way, which
    // it does by magnitude and not by how the caller wrote it.
    assert_eq!(millionths("3e-1"), Some(300_000));
    assert_eq!(millionths("1e-6"), Some(1));
    assert_eq!(millionths("1.5e3"), Some(1_500_000_000));
    assert_eq!(millionths("3E1"), Some(30_000_000));
}

#[test]
fn a_rate_that_is_not_a_usable_number_is_refused() {
    // Each of these would otherwise be coerced into a number nobody wrote, and
    // the run would be priced against it without saying so. The last two are too
    // fine to hold in micro-dollars; the rest are not rates at all.
    for bad in [
        "-3",
        "three",
        "",
        "3.7.5",
        "1_000",
        "1e17",
        "1e-9",
        "3.0000001",
    ] {
        assert_eq!(millionths(bad), None, "{bad:?} must not parse");
    }
}

// --- table parsing --------------------------------------------------------------

#[test]
fn an_unset_table_prices_nothing() {
    for empty in [None, Some(""), Some("   ")] {
        assert_eq!(Pricing::parse(empty), None, "{empty:?} must not price");
    }
    // A syntactically fine but empty table is the same as no table: there is no
    // model in it to match.
    assert_eq!(Pricing::parse(Some("{}")), None);
}

#[test]
fn an_unusable_table_prices_nothing_rather_than_part_of_the_run() {
    for bad in [
        r#"{"m": {"input": 3,}}"#,              // trailing comma
        r#"{"m": 3}"#,                          // rates must be an object
        r#"{"m": {"cache_reads": 0.3}}"#,       // misspelled class
        r#"{"m": {"input": -3}}"#,              // negative rate
        r#"{"m": {"input": 3, "output": ""}}"#, // not a number at all
    ] {
        assert_eq!(Pricing::parse(Some(bad)), None, "{bad} must not price");
    }
}

#[test]
fn a_rate_survives_the_json_path_whatever_form_serde_renders_it_in() {
    // The regression this exists to catch: reading the rate from serde_json's
    // rendering made the answer depend on magnitude, because 1e-6 prints as an
    // exponent and 0.00001 prints in full. The documented sixth decimal place
    // was the one that did not work.
    for (table, expected) in [
        (r#"{"m": {"input": 0.000001}}"#, 0.000_001),
        (r#"{"m": {"input": 0.00001}}"#, 0.000_01),
        (r#"{"m": {"input": 3e-1}}"#, 0.3),
        (r#"{"m": {"input": 0.3}}"#, 0.3),
    ] {
        let pricing = Pricing::parse(Some(table)).unwrap_or_else(|| panic!("{table} must parse"));
        assert_eq!(
            pricing.run_cost_usd(&run("m", totals(1_000_000, 0, 0, 0))),
            Some(expected),
            "{table} must price a million input tokens at its stated rate"
        );
    }
}

#[test]
fn a_model_named_twice_in_different_cases_is_refused() {
    // Both spellings normalize to one key, so one of them would win - and which
    // one is HashMap iteration order, which varies from run to run. Observed
    // before the fix: the same table reported $0.009168 on six runs of ten and
    // $9.168 on the other four.
    let table = r#"{"M": {"input": 1}, "m": {"input": 1000}}"#;
    assert_eq!(
        Pricing::parse(Some(table)),
        None,
        "a bill that changes between runs is worse than no bill"
    );
    // Trimming collides the same way.
    assert_eq!(
        Pricing::parse(Some(r#"{"m": {"input": 1}, " m ": {"input": 2}}"#)),
        None
    );
}

#[test]
fn a_model_id_matches_case_insensitively_after_trimming() {
    let pricing = Pricing::parse(Some(r#"{"  Claude-Sonnet-5 ": {"input": 3}}"#)).unwrap();
    let cost = pricing.run_cost_usd(&run("claude-sonnet-5", totals(1_000_000, 0, 0, 0)));
    assert_eq!(cost, Some(3.0));
}

// --- pricing a run ----------------------------------------------------------------

#[test]
fn each_token_class_is_billed_at_its_own_rate() {
    // The whole reason cache writes were split out of input: at these rates a
    // write costs 12.5x a read, so folding them together is not a rounding error.
    let pricing = Pricing::parse(Some(SONNET)).unwrap();
    let cost = pricing.run_cost_usd(&run("claude-sonnet-5", totals(4085, 509, 6837, 2279)));
    // (4085*3 + 509*15 + 6837*0.3 + 2279*3.75) / 1e6 = $0.03048735, reported to
    // the micro-dollar. Billing those 2279 writes at the plain input rate would
    // report $0.028778 instead - 6% light, and the gap grows every time a lapsed
    // cached prefix is rebuilt.
    assert_eq!(cost, Some(0.030_487));
}

#[test]
fn reasoning_falls_back_to_the_output_rate() {
    // Every provider here bills reasoning as output; afi only reports it apart so
    // the counts stay disjoint. A caller who sets no reasoning rate still gets a
    // right answer.
    let pricing = Pricing::parse(Some(r#"{"m": {"input": 0, "output": 15}}"#)).unwrap();
    let mut usage = totals(0, 1_000_000, 0, 0);
    usage.reasoning_tokens = 1_000_000;
    assert_eq!(pricing.run_cost_usd(&run("m", usage)), Some(30.0));
}

#[test]
fn an_explicit_reasoning_rate_wins_over_the_output_one() {
    let pricing = Pricing::parse(Some(r#"{"m": {"output": 15, "reasoning": 1}}"#)).unwrap();
    let mut usage = totals(0, 0, 0, 0);
    usage.reasoning_tokens = 2_000_000;
    assert_eq!(pricing.run_cost_usd(&run("m", usage)), Some(2.0));
}

#[test]
fn a_model_with_no_entry_is_not_priced_at_zero() {
    let pricing = Pricing::parse(Some(SONNET)).unwrap();
    let cost = pricing.run_cost_usd(&run("claude-opus-5", totals(4085, 509, 0, 0)));
    assert_eq!(cost, None, "an unknown model must report no cost at all");
}

#[test]
fn a_used_class_with_no_rate_suppresses_the_whole_figure() {
    // Pricing four classes and calling the result the total is the exact failure
    // this feature exists to avoid.
    let pricing = Pricing::parse(Some(r#"{"m": {"input": 3, "output": 15}}"#)).unwrap();
    assert_eq!(
        pricing.run_cost_usd(&run("m", totals(100, 20, 0, 2279))),
        None
    );
}

#[test]
fn an_unused_class_with_no_rate_is_harmless() {
    // An OpenAI-compatible source reports 0 cache writes on every request, so
    // requiring a write rate from that caller would suppress every cost figure.
    let pricing = Pricing::parse(Some(r#"{"m": {"input": 3, "output": 15}}"#)).unwrap();
    assert_eq!(
        pricing.run_cost_usd(&run("m", totals(1_000_000, 1_000_000, 0, 0))),
        Some(18.0)
    );
}

#[test]
fn a_run_that_switched_models_bills_each_at_its_own_rates() {
    let pricing = Pricing::parse(Some(
        r#"{"cheap": {"input": 1, "output": 2}, "dear": {"input": 10, "output": 20}}"#,
    ))
    .unwrap();
    let cost = pricing.run_cost_usd(&[
        (billed("cheap"), totals(1_000_000, 1_000_000, 0, 0)),
        (billed("dear"), totals(1_000_000, 1_000_000, 0, 0)),
    ]);
    assert_eq!(cost, Some(33.0));
}

#[test]
fn a_run_that_switched_onto_an_unpriced_model_reports_no_cost() {
    let pricing = Pricing::parse(Some(r#"{"cheap": {"input": 1, "output": 2}}"#)).unwrap();
    let cost = pricing.run_cost_usd(&[
        (billed("cheap"), totals(1_000_000, 1_000_000, 0, 0)),
        (billed("dear"), totals(1_000_000, 1_000_000, 0, 0)),
    ]);
    assert_eq!(cost, None, "half a run's cost is not the run's cost");
}

#[test]
fn a_run_that_reported_no_usage_has_no_cost() {
    let pricing = Pricing::parse(Some(SONNET)).unwrap();
    assert_eq!(pricing.run_cost_usd(&[]), None);
}

#[test]
fn the_figure_is_rounded_to_whole_micro_dollars() {
    // 1 token at $3/M is $0.000003, and a summary reporting float noise around
    // that is unassertable in a workflow.
    let pricing = Pricing::parse(Some(SONNET)).unwrap();
    assert_eq!(
        pricing.run_cost_usd(&run("claude-sonnet-5", totals(1, 0, 0, 0))),
        Some(0.000_003)
    );
    // Half a micro-dollar rounds up rather than vanishing.
    let sub = Pricing::parse(Some(r#"{"m": {"input": 0.5}}"#)).unwrap();
    assert_eq!(
        sub.run_cost_usd(&run("m", totals(1, 0, 0, 0))),
        Some(0.000_001)
    );
}

#[test]
fn a_nonsense_token_count_saturates_instead_of_panicking() {
    // A provider returning u64::MAX must not overflow the arithmetic in a debug
    // build or wrap to a small bill in a release one.
    let pricing = Pricing::parse(Some(SONNET)).unwrap();
    let cost = pricing
        .run_cost_usd(&run(
            "claude-sonnet-5",
            totals(u64::MAX, u64::MAX, u64::MAX, u64::MAX),
        ))
        .unwrap();
    assert!(cost > 0.0);
}

// --- env plumbing -----------------------------------------------------------------

#[test]
fn a_run_that_set_no_rates_is_still_priced() {
    // The point of shipping a table: `AFI_PRICES` used to be the only way to get
    // a figure at all, so every run that had not written one reported nothing.
    let env: HashMap<String, String> = HashMap::new();
    let pricing = Pricing::from_env(&env).expect("the shipped table must be read");
    assert!(
        !pricing.fetched().is_empty(),
        "the shipped table must say when it was projected"
    );
    let billed = vec![(
        Billed {
            source: "anthropic".to_string(),
            provider: Some(Provider::Anthropic),
            model: "claude-sonnet-4-6".to_string(),
        },
        totals(1_000_000, 0, 0, 0),
    )];
    assert_eq!(
        pricing.run_cost_usd(&billed),
        Some(3.0),
        "a million input tokens at the shipped Sonnet rate"
    );
}

#[test]
fn the_callers_own_rates_beat_the_shipped_ones() {
    let mut env: HashMap<String, String> = HashMap::new();
    env.insert(
        "AFI_PRICES".to_string(),
        r#"{"claude-sonnet-4-6": {"input": 99}}"#.to_string(),
    );
    let pricing = Pricing::from_env(&env).expect("a valid table must be read");
    let billed = vec![(
        Billed {
            source: "anthropic".to_string(),
            provider: Some(Provider::Anthropic),
            model: "claude-sonnet-4-6".to_string(),
        },
        totals(1_000_000, 1_000_000, 0, 0),
    )];
    // The override replaces the input rate and leaves the output rate standing.
    // Replacing the whole card instead would leave output unpriced and silence
    // `cost_usd` for the model the override was written to correct.
    assert_eq!(pricing.run_cost_usd(&billed), Some(99.0 + 15.0));
}

#[test]
fn an_unusable_table_still_prices_nothing_at_all() {
    // Unchanged, and it has to stay that way: a caller who wrote a rate wrong
    // must not be quietly billed against the shipped card instead, because the
    // figure would look right and be the wrong one.
    let mut env: HashMap<String, String> = HashMap::new();
    env.insert(
        "AFI_PRICES".to_string(),
        r#"{"m": {"input": -3}}"#.to_string(),
    );
    assert_eq!(Pricing::from_env(&env), None);
}

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
