//! Caller-supplied token rates, so the run summary can report money.
//!
//! No provider afi speaks to returns a cost. Anthropic's Messages API reports
//! tokens, and so does every OpenAI-compatible endpoint, so a cost figure has to
//! come from a price table somewhere. Compiling one into afi puts it where
//! nobody notices it going stale, and a stale table reports a wrong number with
//! total confidence - worse than reporting nothing, because a wrong number gets
//! charted and trusted.
//!
//! The table therefore comes from the caller, in `AFI_PRICES`, next to whoever
//! is accountable for the invoice. A run whose model or token class has no rate
//! gets no `cost_usd` field at all, rather than a zero, a null, or a partial
//! total that quietly under-reports.
//!
//! Arithmetic is integer micro-USD throughout. Rates are held exactly as
//! written and the single division happens at the end, so the figure is
//! reproducible by hand and does not depend on how the run split across turns.

use std::collections::HashMap;
use std::hash::BuildHasher;

use chrono::Utc;
use serde::Deserialize;
use serde_json::Number;

use crate::model::usage_totals::{Billed, UsageTotals};
use crate::sessions;

pub mod catalog;
pub mod provider;
pub(crate) mod refresh;
pub(crate) mod table;

/// The env var carrying the table.
const PRICES_ENV: &str = "AFI_PRICES";

/// One model's rates, in micro-USD per million tokens.
///
/// `None` is "the caller set no rate for this class", which only matters if the
/// run actually spent tokens there.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Rates {
    input: Option<u64>,
    output: Option<u64>,
    cache_read: Option<u64>,
    cache_write: Option<u64>,
    /// Falls back to `output`. Every provider here bills reasoning at the output
    /// rate; afi splits it out only so the reported counts stay disjoint.
    reasoning: Option<u64>,
}

impl Rates {
    /// These rates over `under`'s, class by class.
    ///
    /// Per class rather than wholesale, so naming one negotiated rate keeps the
    /// rest of that model's card. Replacing would make a one-line override
    /// silence `cost_usd` for the model it was meant to correct.
    fn over(self, under: Self) -> Self {
        Self {
            input: self.input.or(under.input),
            output: self.output.or(under.output),
            cache_read: self.cache_read.or(under.cache_read),
            cache_write: self.cache_write.or(under.cache_write),
            reasoning: self.reasoning.or(under.reasoning),
        }
    }

    /// `tokens x rate` summed over the five classes, left undivided.
    ///
    /// Dividing once at the very end is what keeps the total independent of how
    /// the run happened to split across turns and models.
    fn weighted(self, usage: &UsageTotals) -> Option<u128> {
        let classes = [
            (usage.input_tokens, self.input),
            (usage.output_tokens, self.output),
            (usage.cache_read_tokens, self.cache_read),
            (usage.cache_write_tokens, self.cache_write),
            (usage.reasoning_tokens, self.reasoning.or(self.output)),
        ];
        let mut acc: u128 = 0;
        for (tokens, rate) in classes {
            if tokens == 0 {
                // A class nobody used cannot change the bill, so leaving it
                // unpriced must not suppress the whole figure.
                continue;
            }
            acc = acc.saturating_add(u128::from(tokens).saturating_mul(u128::from(rate?)));
        }
        Some(acc)
    }
}

/// What a run's tokens cost, in layers.
///
/// The operator's own rates sit above the table afi ships and refreshes, and
/// they combine class by class rather than replacing: naming a negotiated input
/// rate for a model should not take that model's output rate down with it and
/// silence the figure entirely.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pricing {
    /// `AFI_PRICES` and the `prices` config block. Flat and model-keyed, with no
    /// provider: an operator naming a rate means it, whichever endpoint serves
    /// the model.
    overrides: HashMap<String, Rates>,
    /// Provider, then model. Keyed on the provider because the same model id is
    /// served by several at different rates - see [`provider`].
    by_provider: table::Providers,
    /// The day the layer under the overrides was projected, or empty when there
    /// is no such layer. Read by the footer's staleness warning.
    fetched: String,
}

impl Pricing {
    /// The rates this run bills against: the table afi ships or has refreshed,
    /// with anything in `AFI_PRICES` layered on top.
    ///
    /// `None` only when the overrides are unusable, which disables cost
    /// reporting outright - a half-read table would price part of a run and call
    /// the result the total. A run with no overrides at all still gets a
    /// `Pricing`, which is the whole point of shipping a table.
    #[must_use]
    pub fn from_env<S: BuildHasher>(env: &HashMap<String, String, S>) -> Option<Self> {
        let overrides = read_overrides(env.get(PRICES_ENV).map(String::as_str))?;
        let (by_provider, fetched) = table::layers(&sessions::afi_home(env));
        table::warn_if_stale(&fetched, Utc::now().date_naive(), env);
        Some(Self {
            overrides,
            by_provider,
            fetched,
        })
    }

    /// Parse `AFI_PRICES` alone, with no table under it.
    ///
    /// The rate table as the caller wrote it and nothing else, for a caller
    /// testing what it wrote rather than what a run would bill.
    #[must_use]
    pub fn parse(raw: Option<&str>) -> Option<Self> {
        let overrides = read_overrides(raw)?;
        if overrides.is_empty() {
            return None;
        }
        Some(Self {
            overrides,
            ..Self::default()
        })
    }

    /// The day the shipped or refreshed rates were projected, for a caller that
    /// has to say how old a figure is. Empty when nothing but overrides applies.
    #[must_use]
    pub fn fetched(&self) -> &str {
        &self.fetched
    }

    /// The rates that price one billed entry, or `None` when nothing does.
    fn rates_for(&self, billed: &Billed) -> Option<Rates> {
        let model = key(&billed.model);
        let table = billed
            .provider
            .and_then(|provider| self.by_provider.get(&provider))
            .and_then(|models| models.get(&model))
            .copied();
        match (self.overrides.get(&model).copied(), table) {
            (Some(over), Some(under)) => Some(over.over(under)),
            (over, under) => over.or(under),
        }
    }

    /// The run's cost in USD, priced per entry so a session that switched source
    /// or model is still right.
    ///
    /// `None` when anything that spent tokens has no rates, or no rate for a
    /// class it used. Absent beats approximate: a partial total under-reports
    /// without saying so.
    #[must_use]
    pub fn run_cost_usd(&self, billed: &[(Billed, UsageTotals)]) -> Option<f64> {
        if billed.is_empty() {
            return None;
        }
        let mut weighted: u128 = 0;
        for (who, usage) in billed {
            weighted = weighted.saturating_add(self.rates_for(who)?.weighted(usage)?);
        }
        usd(round_to_micros(weighted))
    }
}

/// The operator's own rates, or `None` when what they wrote is unusable.
///
/// An empty map is "nothing was set", which is not a problem. `None` is "what
/// was set cannot be read", which disables cost reporting for the whole run.
fn read_overrides(raw: Option<&str>) -> Option<HashMap<String, Rates>> {
    let Some(raw) = raw.map(str::trim).filter(|r| !r.is_empty()) else {
        return Some(HashMap::new());
    };
    let table: HashMap<String, RawRates> = match serde_json::from_str(raw) {
        Ok(table) => table,
        Err(error) => {
            eprintln!(
                "afi: ignoring bad {PRICES_ENV} JSON ({error}); no cost_usd will be reported"
            );
            return None;
        }
    };
    normalize(table)
}

/// Check every rate and key the table by normalized model id.
fn normalize(table: HashMap<String, RawRates>) -> Option<HashMap<String, Rates>> {
    let mut by_model = HashMap::with_capacity(table.len());
    for (model, raw_rates) in table {
        let rates = raw_rates
            .to_rates()
            .map_err(|field| report_bad_rate(&model, field))
            .ok()?;
        // Two spellings of one id are refused rather than resolved. `table` is a
        // HashMap, so which of them survived would vary from run to run, and so
        // would the bill - the one failure this whole module exists to prevent.
        if by_model.insert(key(&model), rates).is_some() {
            report_duplicate(&model);
            return None;
        }
    }
    Some(by_model)
}

/// Model ids are matched case-insensitively after trimming, so a hand-written
/// table is not defeated by stray whitespace.
fn key(model: &str) -> String {
    model.trim().to_ascii_lowercase()
}

/// Whole micro-USD, rounded half-up, from the undivided `tokens x rate` sum.
fn round_to_micros(weighted: u128) -> u128 {
    weighted.saturating_add(500_000) / 1_000_000
}

/// Render micro-USD as a plain decimal and read it back as a float.
///
/// The obvious `micros as f64 / 1e6` is a lossy cast the lint policy forbids,
/// and going through the exact decimal is not an approximation of it - every
/// figure afi can produce round-trips.
fn usd(micros: u128) -> Option<f64> {
    format!("{}.{:06}", micros / 1_000_000, micros % 1_000_000)
        .parse::<f64>()
        .ok()
}

fn report_bad_rate(model: &str, field: &str) {
    eprintln!(
        "afi: {PRICES_ENV}[{model:?}].{field} is not a usable rate; \
         want USD per million tokens, not negative, \
         and no finer than six decimal places; \
         no cost_usd will be reported"
    );
}

fn report_duplicate(model: &str) {
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
struct RawRates {
    input: Option<Number>,
    output: Option<Number>,
    cache_read: Option<Number>,
    cache_write: Option<Number>,
    reasoning: Option<Number>,
}

impl RawRates {
    /// Convert every rate present, naming the first bad field so the warning can
    /// point at it.
    fn to_rates(&self) -> Result<Rates, &'static str> {
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
        Some(number) => micros_per_million(&number.to_string())
            .map(Some)
            .ok_or(field),
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
fn micros_per_million(raw: &str) -> Option<u64> {
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

#[cfg(test)]
mod tests;
