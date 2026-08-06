use super::*;

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
    }
}

fn run(model: &str, usage: UsageTotals) -> Vec<(String, UsageTotals)> {
    vec![(model.to_string(), usage)]
}

// --- rate parsing ---------------------------------------------------------------

#[test]
fn a_rate_is_read_as_an_exact_decimal() {
    assert_eq!(micros_per_million("3"), Some(3_000_000));
    assert_eq!(micros_per_million("0.3"), Some(300_000));
    assert_eq!(micros_per_million("3.75"), Some(3_750_000));
    assert_eq!(micros_per_million(".5"), Some(500_000));
    assert_eq!(micros_per_million("0.000001"), Some(1));
}

#[test]
fn a_rate_that_is_not_a_plain_decimal_is_refused() {
    // Each of these would otherwise be coerced into a number nobody wrote, and
    // the run would be priced against it without saying so.
    for bad in ["-3", "1e-9", "3.0000001", "three", "", "3.7.5", "1_000"] {
        assert_eq!(micros_per_million(bad), None, "{bad:?} must not parse");
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
        ("cheap".to_string(), totals(1_000_000, 1_000_000, 0, 0)),
        ("dear".to_string(), totals(1_000_000, 1_000_000, 0, 0)),
    ]);
    assert_eq!(cost, Some(33.0));
}

#[test]
fn a_run_that_switched_onto_an_unpriced_model_reports_no_cost() {
    let pricing = Pricing::parse(Some(r#"{"cheap": {"input": 1, "output": 2}}"#)).unwrap();
    let cost = pricing.run_cost_usd(&[
        ("cheap".to_string(), totals(1_000_000, 1_000_000, 0, 0)),
        ("dear".to_string(), totals(1_000_000, 1_000_000, 0, 0)),
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
fn the_table_comes_from_afi_prices() {
    let mut env: HashMap<String, String> = HashMap::new();
    assert_eq!(Pricing::from_env(&env), None);
    env.insert("AFI_PRICES".to_string(), SONNET.to_string());
    let pricing = Pricing::from_env(&env).expect("a valid table must be read");
    assert!(
        pricing
            .run_cost_usd(&run("claude-sonnet-5", totals(1_000_000, 0, 0, 0)))
            .is_some()
    );
}
