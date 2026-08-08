//! Reading a rate, and rendering money back.
//!
//! Exact decimal throughout. A rate is read from its digits rather than from the
//! float they denote, because the answer would otherwise depend on how
//! `serde_json` chose to print the number - `1e-6` renders as an exponent and
//! `0.00001` does not, so of the six decimal places this promises, only five
//! would work. The same reader takes a budget and a threshold ratio, which is
//! what makes a cap of `2.50` and a rate of `2.50` the same integer by
//! construction rather than by two parsers agreeing.

use serde::Deserialize;
use serde_json::Number;

use super::{PRICES_ENV, Rates};

/// Model ids are matched case-insensitively after trimming, so a hand-written
/// table is not defeated by stray whitespace.
pub(super) fn key(model: &str) -> String {
    model.trim().to_ascii_lowercase()
}

/// Whole micro-USD, rounded half-up, from the undivided `tokens x rate` sum.
pub(super) fn round_to_micros(weighted: u128) -> u128 {
    weighted.saturating_add(500_000) / 1_000_000
}

/// Render micro-USD as a plain decimal and read it back as a float.
///
/// The obvious `micros as f64 / 1e6` is a lossy cast the lint policy forbids,
/// and going through the exact decimal is not an approximation of it - every
/// figure afi can produce round-trips.
pub(crate) fn usd(micros: u128) -> Option<f64> {
    format!("{}.{:06}", micros / 1_000_000, micros % 1_000_000)
        .parse::<f64>()
        .ok()
}

pub(super) fn report_bad_rate(model: &str, field: &str) {
    eprintln!(
        "afi: {PRICES_ENV}[{model:?}].{field} is not a usable rate; \
         want USD per million tokens, not negative, \
         and no finer than six decimal places; \
         no cost_usd will be reported"
    );
}

pub(super) fn report_duplicate(model: &str) {
    eprintln!(
        "afi: {PRICES_ENV} names {:?} twice, counting case and surrounding \
         space as the same id; no cost_usd will be reported",
        key(model)
    );
}

/// The rate classes [`RawRates`] accepts, for a caller that has to check the
/// same shape without deserializing it - the config file, which names the key it
/// refuses. Kept here so one list answers for both.
pub(crate) const RATE_CLASSES: [&str; 5] =
    ["input", "output", "cache_read", "cache_write", "reasoning"];

/// The table as written, before the rates are checked. Unknown keys are an
/// error rather than a silent drop, so a misspelled `cache_reads` is heard
/// about instead of being priced at nothing.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawRates {
    input: Option<Number>,
    output: Option<Number>,
    cache_read: Option<Number>,
    cache_write: Option<Number>,
    reasoning: Option<Number>,
}

impl RawRates {
    /// Convert every rate present, naming the first bad field so the warning can
    /// point at it.
    pub(super) fn to_rates(&self) -> Result<Rates, &'static str> {
        Ok(Rates {
            input: micros(self.input.as_ref(), "input")?,
            output: micros(self.output.as_ref(), "output")?,
            cache_read: micros(self.cache_read.as_ref(), "cache_read")?,
            cache_write: micros(self.cache_write.as_ref(), "cache_write")?,
            reasoning: micros(self.reasoning.as_ref(), "reasoning")?,
        })
    }
}

fn micros(raw: Option<&Number>, field: &'static str) -> Result<Option<u64>, &'static str> {
    match raw {
        None => Ok(None),
        Some(number) => millionths(&number.to_string()).map(Some).ok_or(field),
    }
}

/// Parse a USD-per-million-tokens rate into micro-USD.
///
/// Read from the digits rather than from the float they denote, so `3`, `0.3`,
/// `3e-1`, and `0.000001` all land exactly. Reading it from the rendered form
/// instead would make the answer depend on how `serde_json` chose to print the
/// number, and `1e-6` prints as an exponent while `0.00001` does not - so of
/// the six decimal places this module promises, only five would work.
///
/// Negatives, and anything finer than a micro-dollar or too large to hold, are
/// refused rather than coerced. A rate that fine cannot move a figure afi
/// reports, so a caller who wrote one has made a mistake worth hearing about.
pub(crate) fn millionths(raw: &str) -> Option<u64> {
    let (mantissa, exponent) = split_exponent(raw)?;
    let (whole, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = |part: &str| part.bytes().all(|b| b.is_ascii_digit());
    if (whole.is_empty() && frac.is_empty()) || !digits(whole) || !digits(frac) {
        return None;
    }
    // Where the decimal point falls once the value is scaled to micro-USD.
    let point = i64::try_from(whole.len()).ok()? + i64::from(exponent) + 6;
    scaled(&format!("{whole}{frac}"), point)
}

/// Split `3e-1` into mantissa and exponent. No exponent means 0.
fn split_exponent(raw: &str) -> Option<(&str, i32)> {
    match raw.split_once(['e', 'E']) {
        None => Some((raw, 0)),
        Some((mantissa, exponent)) => Some((mantissa, exponent.parse().ok()?)),
    }
}

/// Read `digits` as a whole number with the decimal point at `point`.
///
/// `None` when anything past the point is non-zero, which is a rate finer than
/// the micro-dollar this is counted in.
fn scaled(digits: &str, point: i64) -> Option<u64> {
    // A u64 holds 20 digits, so a point past that is not a rate afi can use.
    if !(0..=20).contains(&point) {
        return None;
    }
    let point = usize::try_from(point).ok()?;
    if digits.len() > point && digits[point..].bytes().any(|byte| byte != b'0') {
        return None;
    }
    let whole: String = digits.chars().take(point).collect();
    let padded = format!("{whole:0<point$}");
    if padded.is_empty() {
        // Every digit was zero and the point sits left of all of them.
        return Some(0);
    }
    padded.parse().ok()
}
