//! Run-level token accounting.
//!
//! `normalize_usage` yields a per-turn breakdown, but a run spans many turns and
//! a caller reporting on one wants a single set of numbers. Totals live in a
//! process-wide accumulator rather than being threaded through the turn loop,
//! the same shape `log::log_event` already uses: one CLI process is one run.
//!
//! Summing input tokens across turns is deliberate, not double counting. Every
//! turn is a separate billed request that resends the whole history, so the
//! per-turn inputs are what a provider charges for. `input_tokens` excludes the
//! cached prefix and `output_tokens` excludes reasoning, so the four fields are
//! disjoint and add up to the run's billable total.

use std::sync::{Mutex, OnceLock, PoisonError};

use super::stream::NormalizedUsage;

/// Cumulative token counts for one run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageTotals {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub reasoning_tokens: u64,
    /// Model turns that reported usage. A turn the provider gave no numbers for
    /// is not counted, so a caller can tell "no turns" from "no data".
    pub turns: u64,
}

impl UsageTotals {
    /// Fold one turn's normalized usage in.
    pub fn add(&mut self, usage: &NormalizedUsage) {
        self.input_tokens = self.input_tokens.saturating_add(usage.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(usage.output_tokens);
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(usage.cache_read_tokens);
        self.reasoning_tokens = self.reasoning_tokens.saturating_add(usage.reasoning_tokens);
        self.turns = self.turns.saturating_add(1);
    }

    /// Every token the run was billed for. The four fields are disjoint, so this
    /// is their sum rather than a separate provider figure.
    #[must_use]
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.output_tokens)
            .saturating_add(self.cache_read_tokens)
            .saturating_add(self.reasoning_tokens)
    }

    /// Whether any turn reported usage at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.turns == 0
    }
}

fn totals() -> &'static Mutex<UsageTotals> {
    static TOTALS: OnceLock<Mutex<UsageTotals>> = OnceLock::new();
    TOTALS.get_or_init(|| Mutex::new(UsageTotals::default()))
}

/// Record one turn's usage. A poisoned lock recovers rather than panicking: bad
/// accounting must never take down a run.
pub fn record(usage: &NormalizedUsage) {
    let mut guard = totals().lock().unwrap_or_else(PoisonError::into_inner);
    guard.add(usage);
}

/// The run's totals so far.
#[must_use]
pub fn snapshot() -> UsageTotals {
    *totals().lock().unwrap_or_else(PoisonError::into_inner)
}

/// Clear the totals. Exists for tests, which share one process.
pub fn reset() {
    let mut guard = totals().lock().unwrap_or_else(PoisonError::into_inner);
    *guard = UsageTotals::default();
}

#[cfg(test)]
mod tests;
