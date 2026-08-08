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

pub mod catalog;
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
    fn weighted(self, usage: &UsageTotals) -> Result<u128, &'static str> {
        let classes = [
            ("input", usage.input_tokens, self.input),
            ("output", usage.output_tokens, self.output),
            ("cache_read", usage.cache_read_tokens, self.cache_read),
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

    /// Why a run on this model could not be priced, or `None` when it can.
    ///
    /// For a caller that has to know *before* the run whether a cap it was given
    /// can ever fire. Only `input` and `output` are demanded, because those are
    /// the two classes every request spends. The cache classes are checked when
    /// they are actually spent rather than in advance: an OpenAI-compatible
    /// source reports no cache writes at all, and demanding a rate for one would
    /// refuse a configuration that is complete for the endpoint it talks to.
    #[must_use]
    pub fn unpriceable(&self, provider: Option<Provider>, model: &str) -> Option<String> {
        let probe = Billed {
            source: String::new(),
            provider,
            model: model.to_string(),
        };
        let Some(rates) = self.rates_for(&probe) else {
            return Some(format!("no rate for model {model:?}"));
        };
        match (rates.input, rates.output) {
            (Some(_), Some(_)) => None,
            (None, _) => Some(format!("model {model:?} has no \"input\" rate")),
            _ => Some(format!("model {model:?} has no \"output\" rate")),
        }
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

    /// The run's cost so far in whole micro-USD, or why there is no figure.
    ///
    /// [`Self::run_cost_usd`] folds every failure into `None`, because a summary
    /// reports a figure or reports nothing. A spend cap cannot afford that: "no
    /// request has spent yet" is zero and must not stop a run, while "afi cannot
    /// price what was spent" is the one thing a cap must never read as free.
    #[must_use]
    pub fn run_cost(&self, billed: &[(Billed, UsageTotals)]) -> Priced {
        if billed.is_empty() {
            return Priced::Nothing;
        }
        let mut weighted: u128 = 0;
        for (who, usage) in billed {
            let Some(rates) = self.rates_for(who) else {
                return Priced::NoRates(who.model.clone());
            };
            match rates.weighted(usage) {
                Ok(amount) => weighted = weighted.saturating_add(amount),
                Err(class) => {
                    return Priced::NoRate {
                        model: who.model.clone(),
                        class,
                    };
                }
            }
        }
        Priced::Spent(round_to_micros(weighted))
    }

    /// The run's cost in USD, priced per entry so a session that switched source
    /// or model is still right.
    ///
    /// `None` when anything that spent tokens has no rates, or no rate for a
    /// class it used. Absent beats approximate: a partial total under-reports
    /// without saying so.
    #[must_use]
    pub fn run_cost_usd(&self, billed: &[(Billed, UsageTotals)]) -> Option<f64> {
        match self.run_cost(billed) {
            Priced::Spent(micros) => usd(micros),
            _ => None,
        }
    }
}

/// What a run's spend adds up to, or why it does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Priced {
    /// The run's cost so far, in whole micro-USD, rounded half-up once at the
    /// end so the figure does not depend on how the run split across turns.
    Spent(u128),
    /// No request has reported usage, so there is nothing to price. Zero, not
    /// unknown.
    Nothing,
    /// Something that spent tokens has no rates at all - an endpoint afi does
    /// not recognise, or a model nothing carries a rate for.
    NoRates(String),
    /// A model with rates has none for a class it spent on.
    NoRate { model: String, class: &'static str },
}

impl Priced {
    /// The sentence to report when a run cannot be priced, or `None` when it
    /// can. `Nothing` is priceable: it is zero.
    #[must_use]
    pub fn why_not(&self) -> Option<String> {
        match self {
            Self::Spent(_) | Self::Nothing => None,
            Self::NoRates(model) => Some(format!("no rate for model {model:?}")),
            Self::NoRate { model, class } => Some(format!(
                "model {model:?} has no {class:?} rate, and spent there"
            )),
        }
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

#[cfg(test)]
mod tests;
