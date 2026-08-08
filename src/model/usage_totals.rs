//! Run-level token accounting.
//!
//! `normalize_usage` yields a per-turn breakdown, but a run spans many turns and
//! a caller reporting on one wants a single set of numbers. Totals live in a
//! process-wide accumulator rather than being threaded through the turn loop,
//! the same shape `log::log_event` already uses: one CLI process is one run.
//!
//! Summing input tokens across turns is deliberate, not double counting. Every
//! turn is a separate billed request that resends the whole history, so the
//! per-turn inputs are what a provider charges for. `input_tokens` excludes
//! both cached prefixes and `output_tokens` excludes reasoning, so the five
//! fields are disjoint and add up to the run's billable total.
//!
//! The accumulator keys on the source *and* the model each request went to,
//! because a piped session can `/source` its way onto a second of either and
//! none of them are billed at the same rates. `snapshot` folds them together
//! for the summary's flat counts; `snapshot_billed` keeps them apart for
//! pricing.
//!
//! Two counts here are not about tokens: `refused_tool_calls`, what the run asked
//! for and was refused, by policy and by the approval gate. They live beside the
//! token totals because they are the same kind of thing - run-level facts the
//! summary reports and one `reset` clears - and because the alternative is
//! threading a counter through the turn loop.
//!
//! The same switch is why every entry names its *source*. Which credential paid
//! cannot be read off whichever source happens to be active when the run ends: a
//! session that spends on one and then switches would attest to a credential
//! that bought nothing. Only a request that reported usage is recorded, so
//! `billed_sources` names the sources that were actually billed.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

use super::stream::NormalizedUsage;
use crate::pricing::provider::Provider;

/// Cumulative token counts for one run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageTotals {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    /// Prompt tokens written into the cache, kept apart from `input_tokens`
    /// because a write is billed above it. Stays 0 on every provider but
    /// Anthropic, which is the only one that reports the figure.
    pub cache_write_tokens: u64,
    pub reasoning_tokens: u64,
    /// Billed requests that reported usage. A model turn is one, and so is a
    /// compression request, which is why this is not called `turns`. A request the
    /// provider gave no numbers for is not counted, so a caller can tell "nothing
    /// ran" from "the provider said nothing".
    pub requests: u64,
    /// How many of the tokens above afi counted rather than was told.
    ///
    /// A subset marker over the five classes, not a sixth class, so it is
    /// deliberately *not* part of [`Self::total_tokens`]. Zero is the ordinary
    /// case. Anything else means part of any cost computed from these counts
    /// rests on afi's arithmetic rather than the provider's - which is why a
    /// budgeted run stops rather than capping against it.
    pub estimated_tokens: u64,
}

impl UsageTotals {
    /// Fold one request's normalized usage in.
    ///
    /// Crate-internal, like `merge`: the counts are a public shape to read, but
    /// building one is afi's own business and not a semver commitment.
    pub(crate) fn add(&mut self, usage: &NormalizedUsage) {
        self.input_tokens = self.input_tokens.saturating_add(usage.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(usage.output_tokens);
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(usage.cache_read_tokens);
        self.cache_write_tokens = self
            .cache_write_tokens
            .saturating_add(usage.cache_write_tokens);
        self.reasoning_tokens = self.reasoning_tokens.saturating_add(usage.reasoning_tokens);
        self.requests = self.requests.saturating_add(1);
        if usage.estimated {
            self.estimated_tokens = self.estimated_tokens.saturating_add(
                usage
                    .input_tokens
                    .saturating_add(usage.output_tokens)
                    .saturating_add(usage.cache_read_tokens)
                    .saturating_add(usage.cache_write_tokens)
                    .saturating_add(usage.reasoning_tokens),
            );
        }
    }

    /// Fold another model's totals in, for the run-wide flat counts.
    pub(crate) fn merge(&mut self, other: &Self) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(other.cache_read_tokens);
        self.cache_write_tokens = self
            .cache_write_tokens
            .saturating_add(other.cache_write_tokens);
        self.reasoning_tokens = self.reasoning_tokens.saturating_add(other.reasoning_tokens);
        self.requests = self.requests.saturating_add(other.requests);
        self.estimated_tokens = self.estimated_tokens.saturating_add(other.estimated_tokens);
    }

    /// Every token the run was billed for. The five fields are disjoint, so this
    /// is their sum rather than a separate provider figure. `estimated_tokens`
    /// is a marker over those five and is deliberately not among them.
    #[must_use]
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.output_tokens)
            .saturating_add(self.cache_read_tokens)
            .saturating_add(self.cache_write_tokens)
            .saturating_add(self.reasoning_tokens)
    }

    /// Whether any request reported usage at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.requests == 0
    }

    /// Whether any of these counts is afi's own arithmetic rather than a
    /// provider's. A spend cap cannot hold over one of these.
    #[must_use]
    pub fn has_estimates(&self) -> bool {
        self.estimated_tokens > 0
    }
}

/// Who served a request, for the two questions that need to tell them apart.
///
/// `source` answers which credential paid, which is what the summary's `auth`
/// block attributes spend to. `provider` answers whose rate card applies, which
/// is a different question with a different answer: the same model id is served
/// by several providers at different rates, so a ledger keyed on the id alone
/// could not be priced without guessing. `None` is an address afi carries no
/// rates for - a self-hosted llama.cpp, most often.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Billed {
    pub source: String,
    pub provider: Option<Provider>,
    pub model: String,
}

/// What the run has been billed for so far, in first-seen order.
///
/// A `Vec` rather than a map because a run touches one or two entries and the
/// order is worth keeping. One list rather than two: which sources spent used to
/// be tracked separately, which meant nothing could say *which model* a given
/// source had been billed for - exactly what pricing needs.
#[derive(Debug, Default)]
struct Ledger {
    entries: Vec<(Billed, UsageTotals)>,
}

fn ledger() -> &'static Mutex<Ledger> {
    static LEDGER: OnceLock<Mutex<Ledger>> = OnceLock::new();
    LEDGER.get_or_init(|| Mutex::new(Ledger::default()))
}

/// Record one request's usage against the model that served it, the source it
/// was billed to, and the rate card that prices it. A poisoned lock recovers
/// rather than panicking: bad accounting must never take down a run.
pub fn record(source: &str, provider: Option<Provider>, model: &str, usage: &NormalizedUsage) {
    let mut guard = ledger().lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((_, totals)) = guard
        .entries
        .iter_mut()
        .find(|(billed, _)| billed.source == source && billed.model == model)
    {
        totals.add(usage);
        return;
    }
    let mut totals = UsageTotals::default();
    totals.add(usage);
    guard.entries.push((
        Billed {
            source: source.to_string(),
            provider,
            model: model.to_string(),
        },
        totals,
    ));
}

/// The sources that actually spent tokens, in first-seen order.
///
/// Empty when no request reported usage at all, which is a failed or unanswered
/// run rather than a free one. More than one entry means no single credential
/// paid for the run, and the summary reports none rather than picking.
#[must_use]
pub fn billed_sources() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (billed, _) in &ledger()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entries
    {
        if !out.contains(&billed.source) {
            out.push(billed.source.clone());
        }
    }
    out
}

/// The run's totals so far, everything folded together.
#[must_use]
pub fn snapshot() -> UsageTotals {
    total(&snapshot_billed())
}

/// Fold a billed snapshot into one set of counts.
///
/// Takes the snapshot rather than reading the accumulator itself, so a caller
/// that also prices the run derives both from one read. Two reads would let the
/// counts and the cost describe different instants.
#[must_use]
pub fn total(billed: &[(Billed, UsageTotals)]) -> UsageTotals {
    billed
        .iter()
        .fold(UsageTotals::default(), |mut acc, entry| {
            acc.merge(&entry.1);
            acc
        })
}

/// The run's entries grouped by the source that was billed, first-seen order
/// kept within each group and between them.
///
/// Derived from the one flat ledger rather than stored beside it, so the flat
/// counts, the run's cost, and the per-source breakdown are three views of a
/// single read and cannot describe different instants.
///
/// Grouped rather than folded per model. Two sources serving one model id are
/// separate entries here because they can be separate rate cards - see
/// [`Billed::provider`] - so folding them would price one source's tokens at the
/// other's rates.
#[must_use]
pub fn by_source(billed: &[(Billed, UsageTotals)]) -> Vec<(String, Vec<(Billed, UsageTotals)>)> {
    let mut out: Vec<(String, Vec<(Billed, UsageTotals)>)> = Vec::new();
    for entry in billed {
        if let Some((_, group)) = out.iter_mut().find(|(name, _)| *name == entry.0.source) {
            group.push(entry.clone());
            continue;
        }
        out.push((entry.0.source.clone(), vec![entry.clone()]));
    }
    out
}

/// The run's totals split by who served them, for anything that prices them.
#[must_use]
pub fn snapshot_billed() -> Vec<(Billed, UsageTotals)> {
    ledger()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entries
        .clone()
}

/// Tool calls the run refused, split by what refused them.
///
/// Split because the two answer different questions. A policy block means the run
/// asked for a tool the caller had ruled out, which is the signal worth alerting
/// on. An approval denial can mean that too, but a non-interactive run with no
/// `--yolo` denies every mutating call by default, so on its own it reports the
/// configuration as much as the model's behaviour. One number covering both would
/// make an ordinary unattended run indistinguishable from a probed one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RefusedToolCalls {
    /// Blocked by the run's tool policy - at dispatch, or in a batch discarded
    /// before dispatch could rule on it.
    pub by_policy: u64,
    /// Denied at the approval gate, a human answering no or the automatic denial a
    /// run with no terminal falls back to.
    pub by_approval: u64,
}

impl RefusedToolCalls {
    /// Every refused call, whatever refused it.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.by_policy.saturating_add(self.by_approval)
    }

    /// Whether the run got through without being refused anything.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }
}

/// The two counters behind `RefusedToolCalls`. Plain atomics rather than
/// `UsageTotals` fields: a refusal is afi's own observation, not a number a
/// provider reported, and it splits by no model - the dispatch site that sees one
/// does not know which model asked.
static REFUSED_BY_POLICY: AtomicU64 = AtomicU64::new(0);
static REFUSED_BY_APPROVAL: AtomicU64 = AtomicU64::new(0);

/// Count one tool call the policy refused.
///
/// A tool that ran and failed is not one of these. Folding the two together would
/// lose the only signal the run gives that something asked for a tool it was not
/// allowed to have.
pub fn record_policy_refusal() {
    REFUSED_BY_POLICY.fetch_add(1, Ordering::Relaxed);
}

/// Count one tool call the approval gate denied.
pub fn record_approval_denial() {
    REFUSED_BY_APPROVAL.fetch_add(1, Ordering::Relaxed);
}

/// What the run has been refused so far.
#[must_use]
pub fn refused_tool_calls() -> RefusedToolCalls {
    RefusedToolCalls {
        by_policy: REFUSED_BY_POLICY.load(Ordering::Relaxed),
        by_approval: REFUSED_BY_APPROVAL.load(Ordering::Relaxed),
    }
}

/// Clear the totals. Exists for tests, which share one process.
pub fn reset() {
    let mut guard = ledger().lock().unwrap_or_else(PoisonError::into_inner);
    guard.entries.clear();
    REFUSED_BY_POLICY.store(0, Ordering::Relaxed);
    REFUSED_BY_APPROVAL.store(0, Ordering::Relaxed);
}

#[cfg(test)]
mod tests;
