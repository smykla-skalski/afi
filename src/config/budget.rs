//! What one run may spend, and the two points inside it.
//!
//! A budget written into the prompt is text the model reads. The model does not
//! reliably know its own spend, cannot add it up across turns, and anything else
//! in the context - a repository's instruction file, a tool result, the task
//! itself - can argue with it. So the number never reaches the model as an
//! instruction, and what stops the run is the turn loop declining to open
//! another request.
//!
//! Everything here is integer micro-USD, the same unit `pricing` counts in. The
//! thresholds are worked out once, on the way in, so every check afterwards is
//! an integer comparison: `limit as f64 * ratio` is both the lossy cast the lint
//! policy forbids and the one place a cap could round the wrong way.

use std::collections::HashMap;
use std::hash::BuildHasher;

use crate::pricing::{millionths, usd};
use crate::util::nonblank;

/// Where the model is told to converge, as millionths.
const DEFAULT_SOFT: u64 = 800_000;
/// Where the loop stops, as millionths. Below 1 on purpose - see [`Budget`].
const DEFAULT_HARD: u64 = 950_000;

/// One in a million, the unit a ratio is held in.
const ONE: u128 = 1_000_000;

/// What a run may spend, and the two points inside it.
///
/// The hard threshold sits below the budget rather than at it because the
/// request that crosses the line has already been paid for by the time its usage
/// comes back. Stopping *at* the cap would mean stopping *past* it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    limit: u128,
    soft: u128,
    hard: u128,
    soft_ratio: u64,
    hard_ratio: u64,
    /// Which spelling set the cap, so every message names what was typed.
    named: &'static str,
}

impl Budget {
    /// Whether this much spend has reached the point where the model is told to
    /// converge.
    #[must_use]
    pub fn soft_reached(self, micros: u128) -> bool {
        micros >= self.soft
    }

    /// Whether this much spend has reached the point where the run stops.
    #[must_use]
    pub fn hard_reached(self, micros: u128) -> bool {
        micros >= self.hard
    }

    /// The cap in whole micro-USD.
    #[must_use]
    pub fn limit(self) -> u128 {
        self.limit
    }

    /// Which spelling set the cap: `--budget-usd`, or the variable it came from.
    #[must_use]
    pub fn named(self) -> &'static str {
        self.named
    }

    /// The cap in USD, for the summary.
    #[must_use]
    pub fn limit_usd(self) -> Option<f64> {
        usd(self.limit)
    }

    /// The ratios as they were written, for the summary. Reported rather than
    /// the amounts they resolved to, because what a reader wants to check is
    /// what was asked for.
    #[must_use]
    pub fn ratios_usd(self) -> (Option<f64>, Option<f64>) {
        (
            usd(u128::from(self.soft_ratio)),
            usd(u128::from(self.hard_ratio)),
        )
    }
}

/// Resolve the run's budget: `--budget-usd`, then the variables.
///
/// A flag beats a variable, as everywhere. Read here rather than written into
/// the env map because a refusal has to quote the flag's own name, which a later
/// reader of the variable could not do.
///
/// The ratios are validated whether or not a budget is set, and are inert
/// without one. A standing `soft_budget_ratio` in the operator's own file
/// waiting for a per-run `--budget-usd` is the shape this is meant to have, and
/// refusing that would break every interactive run on the machine.
///
/// `pub(crate)` rather than `pub(super)` so it is also the only way a test
/// anywhere in the crate gets a `Budget`. A second constructor for tests could
/// disagree with the one a run uses, which is the one thing a cap fixture must
/// not do. Nothing but `Runtime` should call it in production.
pub(crate) fn resolve_budget<S: BuildHasher>(
    flag: Option<&str>,
    env: &HashMap<String, String, S>,
) -> Result<Option<Budget>, String> {
    let soft_ratio = ratio(env, "AFI_SOFT_BUDGET_RATIO", DEFAULT_SOFT)?;
    let hard_ratio = ratio(env, "AFI_HARD_BUDGET_RATIO", DEFAULT_HARD)?;
    if soft_ratio > hard_ratio {
        return Err(format!(
            "AFI_SOFT_BUDGET_RATIO {} is above AFI_HARD_BUDGET_RATIO {}, so the run would \
             be stopped before it was ever told to converge",
            show(soft_ratio),
            show(hard_ratio)
        ));
    }
    let Some((named, raw)) = named_budget(flag, env) else {
        return Ok(None);
    };
    // Read once, then split the two refusals off the one figure. `amount` is the
    // same read with zero already excluded, so asking it as well would parse
    // twice and leave a reader wondering which of the two decides.
    let Some(limit) = millionths(raw.trim()) else {
        return Err(format!(
            "{named} {raw:?} is not an amount in USD (want dollars, nothing negative, \
             and no finer than a millionth of a dollar)"
        ));
    };
    if limit == 0 {
        return Err(format!(
            "{named} 0 would stop the run before its first request - leave it unset for no cap"
        ));
    }
    let limit = u128::from(limit);
    let hard = scale(limit, hard_ratio);
    // `scale` floors, so a cap small enough that its hard threshold rounds to
    // nothing stops on the pre-flight checkpoint: `hard_reached(0)` is `0 >= 0`,
    // and the run exits 0 reporting success having sent nothing. That is the
    // shape a budget of zero is refused for, reached by writing a very small
    // number instead - and at a low `hard_budget_ratio` "very small" is not that
    // small. Refused rather than floored up to one micro, because a cap this
    // size cannot mean what it says either way.
    if hard == 0 {
        return Err(format!(
            "{named} {raw:?} is too small to enforce: {} of it rounds down to nothing, so the \
             run would stop before its first request - leave it unset for no cap",
            show(hard_ratio)
        ));
    }
    Ok(Some(Budget {
        limit,
        soft: scale(limit, soft_ratio),
        hard,
        soft_ratio,
        hard_ratio,
        named,
    }))
}

/// The budget as it was given, and the name to quote when refusing it.
fn named_budget<'a, S: BuildHasher>(
    flag: Option<&'a str>,
    env: &'a HashMap<String, String, S>,
) -> Option<(&'static str, &'a str)> {
    if let Some(raw) = flag {
        return Some(("--budget-usd", raw));
    }
    // A blank variable is unset, as everywhere - `util::nonblank` is the rule.
    nonblank(env.get("AFI_BUDGET_USD").map(String::as_str)).map(|raw| ("AFI_BUDGET_USD", raw))
}

/// A fraction of the budget, as millionths.
fn ratio<S: BuildHasher>(
    env: &HashMap<String, String, S>,
    name: &'static str,
    default: u64,
) -> Result<u64, String> {
    let Some(raw) = nonblank(env.get(name).map(String::as_str)) else {
        return Ok(default);
    };
    fraction(raw).ok_or_else(|| {
        format!(
            "{name} {raw:?} is not a fraction of the budget \
             (want a number above 0 and at most 1)"
        )
    })
}

/// A fraction of the budget as millionths, or `None` when it is not one.
///
/// The bound in one place. `config::file::value::ratio` refuses the same shape
/// under the config key's name, and a file that accepted what this refused would
/// report the mistake against the wrong spelling.
#[must_use]
pub(crate) fn fraction(raw: &str) -> Option<u64> {
    millionths(raw.trim()).filter(|value| *value > 0 && u128::from(*value) <= ONE)
}

/// An amount in USD as whole micro-USD, or `None` when it is not one.
///
/// Zero is refused: a budget of nothing stops a run before its first request and
/// reports success, which is the one shape a cap must not take by accident.
#[must_use]
pub(crate) fn amount(raw: &str) -> Option<u64> {
    millionths(raw.trim()).filter(|micros| *micros > 0)
}

/// `limit x ratio`, in whole micro-USD. Integer throughout - a threshold that
/// went through an `f64` would land a micro-dollar off on the one comparison
/// that decides whether a run stops.
fn scale(limit: u128, ratio: u64) -> u128 {
    limit.saturating_mul(u128::from(ratio)) / ONE
}

/// A ratio back as the decimal it was, for a message a person reads.
fn show(ratio: u64) -> String {
    usd(u128::from(ratio)).map_or_else(|| ratio.to_string(), |value| format!("{value}"))
}

#[cfg(test)]
mod tests;
