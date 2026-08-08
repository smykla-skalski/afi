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

use crate::model::usage_totals::{Billed, UsageTotals};
use crate::sessions;

use provider::Provider;
pub(crate) use rates::{RATE_CLASSES, millionths, usd};
use rates::{RawRates, key, report_bad_rate, report_duplicate, round_to_micros};

// Crate-internal, unlike `provider`: `Provider` is a field type on the public
// `usage_totals::Billed`, but nothing outside this module names a catalogue. A
// `pub` facade would make the trait's shape a semver commitment and contradict
// the one promise it exists to make - that swapping catalogues is invisible
// above it.
pub(crate) mod catalog;
pub mod provider;
mod rates;
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
    ///
    /// `Err` names the class that stopped it, so a caller reporting the failure
    /// can say which rate is missing rather than that one is.
    ///
    /// `bound` decides what an unpriced class does. A summary reports a figure
    /// or reports nothing, so it asks for [`Bound::Exact`]. A spend cap cannot
    /// afford "nothing" - see [`Bound::Ceiling`].
    fn weighted(self, usage: &UsageTotals, bound: Bound) -> Result<u128, &'static str> {
        // A cached prompt token is a prompt token the provider chose to charge
        // less for, so `input` is its ceiling and never its floor. `cache_write`
        // is the one class that can exceed input - Anthropic bills a write above
        // it - so it has no substitute and stays strict under either bound.
        let ceiling = matches!(bound, Bound::Ceiling);
        let cache_read = self.cache_read.or_else(|| ceiling.then_some(self.input?));
        let classes = [
            ("input", usage.input_tokens, self.input),
            ("output", usage.output_tokens, self.output),
            ("cache_read", usage.cache_read_tokens, cache_read),
            ("cache_write", usage.cache_write_tokens, self.cache_write),
            (
                "reasoning",
                usage.reasoning_tokens,
                self.reasoning.or(self.output),
            ),
        ];
        let mut acc: u128 = 0;
        for (class, tokens, rate) in classes {
            if tokens == 0 {
                // A class nobody used cannot change the bill, so leaving it
                // unpriced must not suppress the whole figure.
                continue;
            }
            let rate = rate.ok_or(class)?;
            acc = acc.saturating_add(u128::from(tokens).saturating_mul(u128::from(rate)));
        }
        Ok(acc)
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
    ///
    /// Compiled only for tests, which is what `pub` was hiding: a run reaches
    /// its rates through [`Self::from_env`], and this shape - overrides with no
    /// table beneath them - is one no run produces.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn parse(raw: Option<&str>) -> Option<Self> {
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

    /// Why a run on this model could not be priced, or `None` when it can.
    ///
    /// For a caller that has to know *before* the run whether a cap it was given
    /// can ever fire. Only `input` and `output` are demanded, because those are
    /// the two classes every request spends, and they are exactly what
    /// [`Bound::Ceiling`] needs - so a run that passes this check can always be
    /// capped, which is the promise the check exists to make.
    ///
    /// The cache classes are deliberately not demanded. An OpenAI-compatible
    /// source reports no cache *writes* at all, and 44% of the models afi ships
    /// rates for carry no cache-*read* rate - most of them embeddings, image and
    /// speech models that never report one. Demanding either would refuse a
    /// configuration that is complete for the endpoint it talks to.
    #[must_use]
    pub fn unpriceable(&self, provider: Option<Provider>, model: &str) -> Option<String> {
        // Not `?`: no rates at all is the *most* unpriceable a model gets, and
        // returning `None` there would report it as priceable.
        let Some(rates) = self.rates_for(provider, model) else {
            return Some(format!("no rate for model {model:?}"));
        };
        match (rates.input, rates.output) {
            (Some(_), Some(_)) => None,
            (None, _) => Some(missing(model, "input")),
            _ => Some(missing(model, "output")),
        }
    }

    /// The rates that price one model on one provider, or `None` when nothing
    /// does.
    fn rates_for(&self, provider: Option<Provider>, model: &str) -> Option<Rates> {
        let model = key(model);
        let table = provider
            .and_then(|provider| self.by_provider.get(&provider))
            .and_then(|models| models.get(&model))
            .copied();
        match (self.overrides.get(&model).copied(), table) {
            (Some(over), Some(under)) => Some(over.over(under)),
            (over, under) => over.or(under),
        }
    }

    /// The most this run can have cost so far, in whole micro-USD, or why there
    /// is no such figure. The question a spend cap asks.
    ///
    /// [`Self::run_cost_usd`] folds every reason into a figure or `None`, because
    /// a summary reports one or the other. A cap cannot afford that: "no request
    /// has spent yet" is zero and must not stop a run, "afi counted these itself"
    /// is a number a cap must not act on, and "afi cannot price what was spent"
    /// is the one thing a cap must never read as free.
    ///
    /// Priced to [`Bound::Ceiling`], so a model with no cache-read rate caps
    /// early rather than killing the run. The figure can therefore exceed
    /// `cost_usd`, which reports nothing at all in that case.
    #[must_use]
    pub fn run_cost(&self, billed: &[(Billed, UsageTotals)]) -> Priced {
        if billed.is_empty() {
            return Priced::Nothing;
        }
        let micros = match self.weigh(billed, Bound::Ceiling) {
            Ok(weighted) => round_to_micros(weighted),
            Err(why) => return Priced::Unpriceable(why),
        };
        if billed.iter().any(|(_, usage)| usage.has_estimates()) {
            return Priced::Estimated(micros);
        }
        Priced::Spent(micros)
    }

    /// `tokens x rate` summed over every billed entry, left undivided, or the
    /// sentence naming what could not be priced.
    ///
    /// The one walk of the ledger. Both callers want the same arithmetic and
    /// differ only in what an unpriced class costs, which is what [`Bound`]
    /// already says - so parameterising here rather than at [`Rates::weighted`]
    /// alone keeps the two from drifting, which is the pair `Bound` exists for.
    fn weigh(&self, billed: &[(Billed, UsageTotals)], bound: Bound) -> Result<u128, String> {
        let mut weighted: u128 = 0;
        for (who, usage) in billed {
            let Some(rates) = self.rates_for(who.provider, &who.model) else {
                return Err(format!("no rate for model {:?}", who.model));
            };
            let amount = rates
                .weighted(usage, bound)
                .map_err(|class| missing(&who.model, class))?;
            weighted = weighted.saturating_add(amount);
        }
        Ok(weighted)
    }

    /// The run's cost in USD, priced per entry so a session that switched source
    /// or model is still right.
    ///
    /// `None` when anything that spent tokens has no rates, or no rate for a
    /// class it used. Absent beats approximate: a partial total under-reports
    /// without saying so, and the ceiling `run_cost` settles for would
    /// over-report. An estimated figure is still reported, because
    /// `usage.estimated_tokens` beside it is what marks it as one.
    #[must_use]
    pub fn run_cost_usd(&self, billed: &[(Billed, UsageTotals)]) -> Option<f64> {
        // Kept rather than folded into `weigh`, which prices an empty ledger at
        // zero: a run that spent nothing has no cost to report, where the cap
        // reads the same ledger as `Nothing` and carries on.
        if billed.is_empty() {
            return None;
        }
        usd(round_to_micros(self.weigh(billed, Bound::Exact).ok()?))
    }
}

/// How to price a class that was spent on and has no rate.
///
/// The summary and the cap want opposite answers, and giving both the same one
/// is what made a budgeted run die mid-turn on a rate it never needed exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bound {
    /// Report a figure or report nothing. An unpriced class suppresses the whole
    /// total, because a partial one under-reports without saying so.
    Exact,
    /// The most this can have cost. An unpriced cache read is billed at the
    /// model's own `input` rate, which is its ceiling: the provider charges less
    /// for a cached prompt token, never more.
    ///
    /// A cap wants this. Stopping a run early is a cost the operator asked for
    /// when they set a budget; failing the run because afi lacks a discount rate
    /// is not, and refusing to start over one would refuse 44% of the models afi
    /// ships rates for - most of which never report a cache read at all.
    Ceiling,
}

/// A model that has rates but not the one it spent on.
fn missing(model: &str, class: &str) -> String {
    format!("model {model:?} has no {class:?} rate, and spent there")
}

/// What a run's spend adds up to, or why it does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Priced {
    /// The run's cost so far, in whole micro-USD, rounded half-up once at the
    /// end so the figure does not depend on how the run split across turns.
    Spent(u128),
    /// The same figure, over counts afi produced itself because nobody else did.
    ///
    /// Reportable and not cappable. The chars-per-token fallback records no
    /// input tokens at all, so a run capped against this would over-run by
    /// roughly the whole prompt while looking exactly as confident.
    Estimated(u128),
    /// No request has reported usage, so there is nothing to price. Zero, not
    /// unknown.
    Nothing,
    /// Something that spent tokens cannot be priced, and the sentence saying so.
    Unpriceable(String),
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

#[cfg(test)]
mod tests;
