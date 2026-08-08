//! What the run was allowed to spend, and what enforcing that did.
//!
//! Present whenever a budget was set, absent entirely when none was. The block's
//! *existence* answers "was this run capped at all" - a binary fact a consumer
//! cannot get any other way - and the `false` inside answers "did the cap fire",
//! which is the `refused_tool_calls` case exactly: a `false` there is a fact
//! rather than a guess. Always-present would put an object of nulls in nearly
//! every summary afi writes.

use serde_json::{Number, Value, json};

use crate::cost::Outcome;
use crate::pricing::usd;

/// The `usage.budget` object, or `None` when the run carried no cap.
pub(super) fn json(outcome: Option<Outcome>) -> Option<Value> {
    let outcome = outcome?;
    let (soft, hard) = outcome.budget.ratios_usd();
    Some(json!({
        "limit_usd": money(outcome.budget.limit_usd()),
        "soft_ratio": money(soft),
        "hard_ratio": money(hard),
        // The same figure as `cost_usd`, from the same ledger read the whole
        // object is built from - repeated here so a consumer reading the cap
        // never has to look outside it, and because `cost_usd` vanishes on an
        // unpriced run while this is always present.
        "spent_usd": money(outcome.spent.and_then(usd)),
        "converged": outcome.converged,
        "stopped": outcome.stopped,
    }))
}

/// A figure as JSON, or null when there is not one.
///
/// `Number::from_f64` refuses an infinity or a NaN, which no figure afi computes
/// can be - but a null here is a readable absence, and a panic is not.
fn money(value: Option<f64>) -> Value {
    value
        .and_then(Number::from_f64)
        .map_or(Value::Null, Value::Number)
}
