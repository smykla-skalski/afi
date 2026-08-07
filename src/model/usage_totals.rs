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
//! The accumulator keys on the model each request went to, because a piped
//! session can `/source` its way onto a second one and the two are not billed
//! at the same rates. `snapshot` folds the models together for the summary's
//! flat counts; `snapshot_by_model` keeps them apart for pricing.
//!
//! Two counts here are not about tokens: `refused_tool_calls`, what the run asked
//! for and was refused, by policy and by the approval gate. They live beside the
//! token totals because they are the same kind of thing - run-level facts the
//! summary reports and one `reset` clears - and because the alternative is
//! threading a counter through the turn loop.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

use super::stream::NormalizedUsage;

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
    }

    /// Every token the run was billed for. The five fields are disjoint, so this
    /// is their sum rather than a separate provider figure.
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
}

/// Per-model totals in first-seen order. A `Vec` rather than a map because a run
/// touches one or two models and the order is worth keeping.
fn totals() -> &'static Mutex<Vec<(String, UsageTotals)>> {
    static TOTALS: OnceLock<Mutex<Vec<(String, UsageTotals)>>> = OnceLock::new();
    TOTALS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Record one request's usage against the model that served it. A poisoned lock
/// recovers rather than panicking: bad accounting must never take down a run.
pub fn record(model: &str, usage: &NormalizedUsage) {
    let mut guard = totals().lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((_, totals)) = guard.iter_mut().find(|(name, _)| name == model) {
        totals.add(usage);
        return;
    }
    let mut totals = UsageTotals::default();
    totals.add(usage);
    guard.push((model.to_string(), totals));
}

/// The run's totals so far, every model folded together.
#[must_use]
pub fn snapshot() -> UsageTotals {
    total(&snapshot_by_model())
}

/// Fold a per-model snapshot into one set of counts.
///
/// Takes the snapshot rather than reading the accumulator itself, so a caller
/// that also prices the run derives both from one read. Two reads would let the
/// counts and the cost describe different instants.
#[must_use]
pub fn total(by_model: &[(String, UsageTotals)]) -> UsageTotals {
    by_model
        .iter()
        .fold(UsageTotals::default(), |mut acc, entry| {
            acc.merge(&entry.1);
            acc
        })
}

/// The run's totals split by model, for anything that prices them.
#[must_use]
pub fn snapshot_by_model() -> Vec<(String, UsageTotals)> {
    totals()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
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
    totals()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clear();
    REFUSED_BY_POLICY.store(0, Ordering::Relaxed);
    REFUSED_BY_APPROVAL.store(0, Ordering::Relaxed);
}

#[cfg(test)]
mod tests;
