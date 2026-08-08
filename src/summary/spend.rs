//! What each source spent, and how that reaches the summary.
//!
//! Split from `summary.rs` for the reason [`super::auth`] is: the entry and the
//! block it renders stay together, so a field added here has one place to be
//! written out.
//!
//! The run's flat counts answer "what did this cost"; they cannot answer "whose
//! budget paid", because a session that `/source`-switches spends on two. `auth`
//! reports that when one credential paid for everything and declines when two
//! did - declining is honest, but it leaves the auditor with the question they
//! opened the summary for. This is that question answered: one entry per source
//! that was billed, its own counts, its own cost, and the credential that bought
//! them.

use serde_json::{Value, json};

use super::{RunAuth, counts_json};
use crate::model::usage_totals::UsageTotals;

/// One source's share of a run.
///
/// Only sources that were actually billed get one. A source that was configured
/// and never sent a request has nothing to report, and an entry of zeros would
/// read as one that ran for free.
#[derive(Debug, Clone)]
pub struct SourceSpend<'a> {
    /// The source's name, as `/source` and the top-level `source` field spell it.
    pub source: String,
    /// This source's counts alone. Every entry's counts sum to the run's flat
    /// `usage`, since each billed request is recorded against exactly one source.
    pub usage: UsageTotals,
    /// What this source's tokens cost, priced at the rates of the models *it*
    /// served them with. `None` leaves the key out, the same way the run-level
    /// figure does, and for the same reason: a zero reads as free.
    pub cost_usd: Option<f64>,
    /// The credential that paid for this entry. Unlike the run-level `auth` this
    /// is never ambiguous - the whole point of the split - so `None` here means
    /// only that the source is no longer in the runtime to ask.
    pub auth: Option<RunAuth<'a>>,
}

impl SourceSpend<'_> {
    /// The `sources` array, in the order the run first spent on each.
    ///
    /// An array rather than an object keyed by name, because the order is a fact
    /// about the run - which endpoint it started billing on - and a JSON object
    /// does not promise to keep it.
    ///
    /// Empty when nothing was billed, rather than null. `usage` is null there to
    /// keep a silent provider distinguishable from a free run, but an empty list
    /// has no zero row to be misread: it says no source was billed, which is the
    /// whole of what a caller needs, and it iterates like any other.
    #[must_use]
    pub fn json(spend: &[Self]) -> Value {
        Value::Array(spend.iter().map(Self::entry_json).collect())
    }

    /// One entry: which source, what it spent, and who paid.
    ///
    /// `usage` holds no `refused_tool_calls` counts, which the flat block does.
    /// A refusal is afi's own observation about a call that was never sent, so
    /// there is no billed request behind it and no source it belongs to.
    fn entry_json(&self) -> Value {
        json!({
            "source": self.source,
            "usage": counts_json(&self.usage, self.cost_usd),
            "auth": RunAuth::json(self.auth),
        })
    }
}

#[cfg(test)]
mod tests;
