use std::collections::HashMap;

use super::resolve;

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

fn budget(flag: Option<&str>, pairs: &[(&str, &str)]) -> super::Budget {
    resolve(flag, &env(pairs))
        .expect("must resolve")
        .expect("must be set")
}

fn refusal(flag: Option<&str>, pairs: &[(&str, &str)]) -> String {
    resolve(flag, &env(pairs)).expect_err("must refuse")
}

#[test]
fn a_cap_is_read_as_an_exact_decimal() {
    // The same reader the rate table uses, so `--budget-usd 2.50` and a rate of
    // `2.50` are the same integer by construction rather than by two parsers
    // agreeing about a float.
    assert_eq!(budget(Some("5"), &[]).limit(), 5_000_000);
    assert_eq!(budget(Some("2.50"), &[]).limit(), 2_500_000);
    assert_eq!(budget(Some("0.000001"), &[]).limit(), 1);
    assert_eq!(budget(Some(" 5 "), &[]).limit(), 5_000_000);
}

#[test]
fn an_amount_afi_cannot_hold_exactly_is_refused() {
    // Each of these would otherwise be coerced into a number nobody wrote, and
    // the run would be capped against it without saying so.
    for bad in ["-1", "five", "3.7.5", "0.0000001", "1e30", ""] {
        let why = refusal(Some(bad), &[]);
        assert!(why.starts_with("--budget-usd"), "{bad:?} -> {why}");
        assert!(why.contains("amount in USD"), "{bad:?} -> {why}");
    }
}

#[test]
fn a_cap_of_nothing_is_refused_rather_than_honoured() {
    // It would stop the run before its first request and report success, which
    // is the one shape a cap must not be able to take by accident.
    let why = refusal(Some("0"), &[]);
    assert!(why.contains("leave it unset for no cap"), "{why}");
}

#[test]
fn the_flag_beats_the_variable_and_the_message_says_which() {
    let with_both = env(&[("AFI_BUDGET_USD", "10")]);
    assert_eq!(
        resolve(Some("2"), &with_both).unwrap().unwrap().limit(),
        2_000_000
    );
    // And a refusal quotes what was actually typed, which is the reason the flag
    // is read here rather than written into the env map for a later reader.
    assert!(refusal(Some("x"), &[]).starts_with("--budget-usd"));
    assert!(refusal(None, &[("AFI_BUDGET_USD", "x")]).starts_with("AFI_BUDGET_USD"));
}

#[test]
fn a_blank_variable_is_no_cap_at_all() {
    for blank in ["", "   "] {
        assert_eq!(resolve(None, &env(&[("AFI_BUDGET_USD", blank)])), Ok(None));
    }
    assert_eq!(resolve(None, &env(&[])), Ok(None));
}

#[test]
fn the_thresholds_are_exact_integers() {
    // No float sits between the number a caller wrote and the request that was
    // refused, so a cap is the same integer on every machine.
    let budget = budget(Some("5"), &[]);
    assert!(!budget.soft_reached(3_999_999));
    assert!(budget.soft_reached(4_000_000), "0.8 of $5 is exactly $4.00");
    assert!(!budget.hard_reached(4_749_999));
    assert!(
        budget.hard_reached(4_750_000),
        "0.95 of $5 is exactly $4.75"
    );
}

#[test]
fn the_defaults_are_the_ones_the_documentation_states() {
    let (soft, hard) = budget(Some("1"), &[]).ratios_usd();
    assert_eq!((soft, hard), (Some(0.8), Some(0.95)));
}

#[test]
fn a_ratio_outside_the_budget_is_refused() {
    for (name, bad) in [
        ("AFI_SOFT_BUDGET_RATIO", "0"),
        ("AFI_SOFT_BUDGET_RATIO", "1.5"),
        ("AFI_SOFT_BUDGET_RATIO", "-0.5"),
        ("AFI_HARD_BUDGET_RATIO", "2"),
        ("AFI_HARD_BUDGET_RATIO", "half"),
    ] {
        let why = refusal(Some("5"), &[(name, bad)]);
        assert!(why.starts_with(name), "{name}={bad} -> {why}");
        assert!(why.contains("fraction of the budget"), "{why}");
    }
    // 1 is the boundary and is allowed: stopping exactly at the cap is a choice,
    // not a mistake.
    assert!(resolve(Some("5"), &env(&[("AFI_HARD_BUDGET_RATIO", "1")])).is_ok());
}

#[test]
fn a_soft_threshold_above_the_hard_one_is_refused() {
    let why = refusal(
        Some("5"),
        &[
            ("AFI_SOFT_BUDGET_RATIO", "0.99"),
            ("AFI_HARD_BUDGET_RATIO", "0.95"),
        ],
    );
    assert!(why.contains("before it was ever told to converge"), "{why}");
}

#[test]
fn a_ratio_with_no_budget_is_checked_and_inert() {
    // A standing preference in the operator's own file, waiting for a per-run
    // `--budget-usd`, is the shape this is meant to have. Refusing it would
    // break every interactive run on that machine.
    assert_eq!(
        resolve(None, &env(&[("AFI_SOFT_BUDGET_RATIO", "0.5")])),
        Ok(None)
    );
    // A typo is still a typo, whether or not a cap happens to be set today.
    assert!(resolve(None, &env(&[("AFI_SOFT_BUDGET_RATIO", "9")])).is_err());
}

#[test]
fn the_cap_reports_itself_in_the_units_a_person_reads() {
    let budget = budget(Some("2.5"), &[]);
    assert_eq!(budget.limit_usd(), Some(2.5));
    assert_eq!(budget.named(), "--budget-usd");
}
