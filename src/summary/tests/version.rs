//! What the version key promises a consumer, and the shape it stands for.
//!
//! Split out from the rest because it is about the object rather than about the
//! run: everything in `super` asserts what one field reports, and these assert
//! that a reader can tell which fields to expect at all.

use super::shape::sorted_keys;
use super::{summary, totals};
use std::collections::HashMap;

use crate::config::budget::resolve_budget;
use crate::cost::Outcome;
use crate::model::usage_totals::UsageTotals;
use crate::summary::{ErrorKind, RunSummary, SCHEMA_VERSION};
use serde_json::Value;

#[test]
fn every_kind_of_run_names_the_shape_it_is_in() {
    // One unconditional field, asserted on all three kinds of run, because the
    // two that report almost nothing are the ones that need it. A refusal is
    // nothing but absent fields - no source, no model, no usage - so without a
    // version it reads exactly like an afi too old to report any of them.
    let finished = summary(true, "APPROVE", totals(3)).to_json();
    assert_eq!(finished["schema_version"], SCHEMA_VERSION);

    let refused =
        RunSummary::refused("--disallowed-tools needs a value", ErrorKind::Policy).to_json();
    assert_eq!(refused["schema_version"], SCHEMA_VERSION);

    let mut run = summary(false, "", UsageTotals::default());
    run.error = Some("HTTP 401: authentication_error");
    run.error_kind = Some(ErrorKind::Auth);
    assert_eq!(run.to_json()["schema_version"], SCHEMA_VERSION);
}

#[test]
fn the_shape_the_version_stands_for_is_pinned() {
    // The number is only worth reading if it moves when the shape does, and this
    // is the shape it currently stands for. Removing or renaming a key here
    // breaks a consumer that worked, so it comes with a bump and a docs change.
    // Adding one does not - a consumer ignores what it does not know - but it
    // still has to be written down here to be a decision rather than an
    // accident.
    let json = summary(true, "x", totals(3)).to_json();
    assert_eq!(
        sorted_keys(&json),
        [
            "answer",
            "auth",
            "effort",
            "elapsed_secs",
            "error",
            "error_kind",
            "instructions",
            "model",
            "ok",
            "schema_version",
            "source",
            "sources",
            "system_prompt",
            "tools",
            "usage",
        ]
    );
    // `usage` is read field by field like the object around it, so its keys are
    // as much a part of the published shape. `cost_usd` is the one that comes
    // and goes, by design - an unpriced run has no key at all rather than a zero.
    assert_eq!(
        sorted_keys(&json["usage"]),
        [
            "cache_read_tokens",
            "cache_write_tokens",
            "estimated_tokens",
            "input_tokens",
            "output_tokens",
            "reasoning_tokens",
            "refused_by_approval",
            "refused_by_policy",
            "refused_tool_calls",
            "requests",
            "total_tokens",
        ]
    );
    assert_eq!(
        SCHEMA_VERSION, 1,
        "a bump is a promise to consumers; move docs/reference.md with it"
    );
}

/// A published key, the check its type has to keep passing, and what to call
/// that type when it stops.
type Pinned = (&'static str, fn(&Value) -> bool, &'static str);

#[test]
fn the_types_the_shape_promises_are_pinned_too() {
    // Names are half of it. A field that turns from a number into a string
    // breaks a consumer as thoroughly as one that disappears, and it breaks it
    // silently, so the version has to move for that too - which means something
    // has to notice. The fields null here by design are pinned where their
    // content is: `error` and `error_kind` in `super`, `effort` beside the
    // source that resolved it.
    let json = summary(true, "done", totals(3)).to_json();
    let pinned: [Pinned; 11] = [
        ("schema_version", Value::is_u64, "a number"),
        ("ok", Value::is_boolean, "a boolean"),
        ("source", Value::is_string, "a string"),
        ("model", Value::is_string, "a string"),
        ("answer", Value::is_string, "a string"),
        ("elapsed_secs", Value::is_f64, "a number"),
        ("tools", Value::is_array, "an array"),
        ("auth", Value::is_object, "an object"),
        // An array on every run, empty when nothing was billed - a consumer
        // iterates it without checking for null first.
        ("sources", Value::is_array, "an array"),
        ("system_prompt", Value::is_object, "an object"),
        // An array on every run, empty included. A consumer that reads it as a
        // list must not have to handle a null for the ordinary case of a run that
        // loaded no project instructions.
        ("instructions", Value::is_array, "an array"),
    ];
    for (name, holds, expected) in pinned {
        assert!(holds(&json[name]), "{name} must stay {expected}: {json}");
    }
    // Every count in `usage` is one a consumer adds up or charts, so a string
    // among them is the change that would be read as a zero. Two keys are not
    // counts and are excepted by name rather than by the fixture happening not
    // to set them: `cost_usd` is money, and `budget` is an object.
    for (name, count) in json["usage"].as_object().expect("an object") {
        if name == "cost_usd" || name == "budget" {
            continue;
        }
        assert!(count.is_u64(), "usage.{name} must stay a number: {count}");
    }
}

#[test]
fn an_uncapped_run_carries_no_budget_key_at_all() {
    // The key's presence is itself the answer to "was this run capped", so a
    // consumer branches on it and it has to mean exactly that.
    let uncapped = summary(true, "x", totals(3)).to_json();
    assert!(
        uncapped["usage"].get("budget").is_none(),
        "an uncapped run must carry no budget key at all: {uncapped}"
    );
}

#[test]
fn the_budget_block_is_pinned_too() {
    let mut run = summary(true, "x", totals(3));
    run.budget = Some(capped());
    let json = run.to_json();
    assert_eq!(
        sorted_keys(&json["usage"]["budget"]),
        [
            "converged",
            "hard_ratio",
            "limit_usd",
            "soft_ratio",
            "spent_usd",
            "stopped",
        ]
    );
    let block = &json["usage"]["budget"];
    let reported = [
        block["limit_usd"].as_f64(),
        block["soft_ratio"].as_f64(),
        block["hard_ratio"].as_f64(),
        block["spent_usd"].as_f64(),
    ];
    assert_eq!(
        reported,
        [Some(5.0), Some(0.8), Some(0.95), Some(4.83)],
        "the cap, both thresholds as they were written, and the spend: {json}"
    );
    assert_eq!(
        [block["stopped"].as_bool(), block["converged"].as_bool()],
        [Some(true), Some(true)]
    );
}

#[test]
fn a_cap_that_never_measured_anything_reports_a_null_rather_than_a_zero() {
    // A run stopped because it could not be priced spent an unknown amount, and
    // a zero there would read as "this run was free" - the same failure
    // `cost_usd` is absent to avoid.
    let mut run = summary(true, "x", totals(3));
    run.budget = Some(Outcome {
        spent: None,
        stopped: false,
        ..capped()
    });
    let json = run.to_json();
    assert!(json["usage"]["budget"]["spent_usd"].is_null(), "{json}");
    assert_eq!(json["usage"]["budget"]["stopped"], false);
}

/// A $5 cap that stopped a run at $4.83, at the default thresholds.
fn capped() -> Outcome {
    Outcome {
        budget: resolve_budget(Some("5"), &HashMap::new()).unwrap().unwrap(),
        spent: Some(4_830_000),
        converged: true,
        stopped: true,
    }
}
