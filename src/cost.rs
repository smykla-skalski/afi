//! Enforcing what a run may spend.
//!
//! The cap is mechanical, and that is the whole design. A budget written into
//! the prompt is text the model reads: it does not reliably know its own spend,
//! cannot add it up across turns, and anything else in the context - a
//! repository's instruction file, a tool result, the task itself - can argue
//! with it. So the number never reaches the model as an instruction. What
//! reaches the model is one sentence at the soft threshold; what stops the run
//! at the hard one is the turn loop declining to open another request.
//!
//! The state is process-wide, the shape `usage_totals` already argues for: one
//! CLI process is one run. Half the input is that accumulator, and the other
//! half - whether the converge note has been sent, whether the run has stopped -
//! has to outlive a single `run_model_turn_loop`, because a piped session runs
//! one per user turn and `/recover` runs another. Loop-local state would get
//! "once per run" wrong by construction.
//!
//! All the logic lives on [`Guard`], which takes the ledger snapshot as an
//! argument, so every case is unit-tested against a hand-built ledger and
//! nothing has to own the process to check it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

use crate::config::Budget;
use crate::model::usage_totals::{self, Billed, UsageTotals};
use crate::pricing::{Priced, Pricing, usd};

/// What the run's spend allows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// No budget was set: every run before this feature, and every run that
    /// sets none.
    Unlimited,
    /// Under the soft threshold. Nothing to say.
    Under,
    /// The first request to reach the soft threshold. Never returned twice.
    Soft(Crossing),
    /// At or past the hard threshold. No further request may be made.
    Hard(Crossing),
    /// A budget is set and the run cannot be priced, so the cap could never
    /// fire. Carries the sentence to report.
    Unpriceable(String),
}

/// The two figures a threshold message names, in whole micro-USD.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Crossing {
    pub spent: u128,
    pub limit: u128,
}

impl Crossing {
    /// `$4.83 of $5.00`.
    ///
    /// Rendered from the integers, because `{:.2}` on a float would print a
    /// sub-cent budget as `$0.00` - and a cap of $0.005 is a thing a test
    /// harness sets.
    #[must_use]
    pub(crate) fn describe(self) -> String {
        format!("{} of {}", money(self.spent), money(self.limit))
    }
}

/// One amount as dollars, trimmed to at least two places.
fn money(micros: u128) -> String {
    let Some(exact) = usd(micros) else {
        return format!("{micros}\u{b5}$");
    };
    let plain = format!("{exact}");
    match plain.split_once('.') {
        Some((_, frac)) if frac.len() >= 2 => format!("${plain}"),
        Some((whole, frac)) => format!("${whole}.{frac:0<2}"),
        None => format!("${plain}.00"),
    }
}

/// What a run was allowed to spend, and what enforcing it did.
///
/// No spend figure rides here. The guard's last checkpoint runs *before* a turn,
/// so on a run that finished normally it predates the final request - a
/// one-turn run would report having spent nothing. The summary prices the ledger
/// it already reads for `cost_usd` instead, so the two describe one instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    pub budget: Budget,
    /// Whether the converge note was sent. `false` on a run that went from
    /// under the soft threshold to past the hard one in a single turn, which
    /// one large turn does routinely - the note is best effort, the stop is not.
    pub converged: bool,
    /// Whether the loop stopped because the cap was reached. Not set by a run
    /// that stopped because it could not be priced; `error_kind` reports that.
    pub stopped: bool,
}

/// The budget and the rates to enforce it with, plus what enforcing it has done.
#[derive(Debug)]
struct Guard {
    budget: Budget,
    pricing: Pricing,
    converged: bool,
    stopped: bool,
}

impl Guard {
    /// The verdict for one reading of the ledger, latching what it consumes.
    ///
    /// `Soft` is returned to the first request that crosses and never again,
    /// because the converge note is one line once per run. `Hard` latches too,
    /// so the summary still reports a stopped run after the loop has returned
    /// without asking a second time.
    fn checkpoint(&mut self, billed: &[(Billed, UsageTotals)]) -> Verdict {
        let spent = match self.pricing.run_cost(billed) {
            Priced::Spent(micros) => micros,
            Priced::Nothing => 0,
            Priced::Estimated(_) => {
                return Verdict::Unpriceable(
                    "the budget cannot be measured: this endpoint reported no usage, so afi \
                     counted the tokens itself and a cap cannot hold over a guess"
                        .to_string(),
                );
            }
            Priced::Unpriceable(why) => {
                return Verdict::Unpriceable(format!("the budget cannot be measured: {why}"));
            }
        };
        let at = Crossing {
            spent,
            limit: self.budget.limit(),
        };
        if self.budget.hard_reached(spent) {
            self.stopped = true;
            // Returned every time, not just the first. A piped session starts a
            // fresh `run_model_turn_loop` for each user turn, and the loop acts
            // only on `Hard` - so answering a stopped run with anything else
            // would let the turn after it open a request and spend past the cap.
            return Verdict::Hard(at);
        }
        if self.budget.soft_reached(spent) && !self.converged {
            self.converged = true;
            return Verdict::Soft(at);
        }
        Verdict::Under
    }

    /// Whether another request may be opened, consuming nothing.
    ///
    /// Prices the ledger rather than reading [`Self::stopped`]. That flag is
    /// written only by [`Self::checkpoint`], which only the turn loop calls, and
    /// only at the *top* of an iteration - so a turn that finished never set it,
    /// however far past the cap it went. Asking the flag let every spender
    /// outside the loop through: two turns over budget, then `/compress` billing
    /// on for as long as anyone typed it.
    ///
    /// The flag is still consulted first, because a cap that has fired stays
    /// fired even if the ledger were somehow to read lower afterwards.
    fn may_spend(&self, billed: &[(Billed, UsageTotals)]) -> bool {
        if self.stopped {
            return false;
        }
        match self.pricing.run_cost(billed) {
            Priced::Spent(micros) => !self.budget.hard_reached(micros),
            Priced::Nothing => true,
            // Measured by nobody, or priced by nothing: a cap that cannot be
            // computed cannot be honoured, and spending anyway is the one thing
            // it must never do. The turn loop reports this as a failed run; a
            // caller here only needs to not send.
            Priced::Estimated(_) | Priced::Unpriceable(_) => false,
        }
    }
}

/// Fast path for the run that set no budget, which is nearly every run: one
/// relaxed load rather than a mutex.
static ARMED: AtomicBool = AtomicBool::new(false);

fn guard() -> &'static Mutex<Option<Guard>> {
    static GUARD: OnceLock<Mutex<Option<Guard>>> = OnceLock::new();
    GUARD.get_or_init(|| Mutex::new(None))
}

/// Arm the run's budget, clearing anything a previous one left.
///
/// One CLI process is one run - the justification `usage_totals` sets out - so
/// this is called once at startup, and by tests, which share one process. A
/// budget with no rates to measure it never reaches here: `config::refusals`
/// stops that run before it starts.
pub(crate) fn install(budget: Option<Budget>, pricing: Option<&Pricing>) {
    let armed = match (budget, pricing) {
        (Some(budget), Some(pricing)) => Some(Guard {
            budget,
            pricing: pricing.clone(),
            converged: false,
            stopped: false,
        }),
        _ => None,
    };
    ARMED.store(armed.is_some(), Ordering::Relaxed);
    *guard().lock().unwrap_or_else(PoisonError::into_inner) = armed;
}

/// Clear the budget. Exists for tests, which share one process - and compiled
/// only for them, which is what `pub` was hiding: no run clears a budget, it
/// installs one at startup and lives with it.
#[cfg(test)]
pub(crate) fn reset() {
    install(None, None);
}

/// What the run's spend allows, consuming what it reports.
///
/// Latching: only the turn loop can deliver a converge note, so only the turn
/// loop may consume one. Everything else that spends asks [`may_spend`].
#[must_use]
pub(crate) fn checkpoint() -> Verdict {
    if !ARMED.load(Ordering::Relaxed) {
        return Verdict::Unlimited;
    }
    let billed = usage_totals::snapshot_billed();
    let mut held = guard().lock().unwrap_or_else(PoisonError::into_inner);
    held.as_mut()
        .map_or(Verdict::Unlimited, |guard| guard.checkpoint(&billed))
}

/// Whether a request may still be made against the run's budget.
///
/// Read-only and consumes nothing, for everything that spends outside the turn
/// loop - `/compress` today, whatever is added next. It prices the ledger
/// itself rather than trusting the loop to have looked recently, because the
/// loop's last look predates the turn that finished.
///
/// The ledger is read before the guard is locked, the same order
/// [`checkpoint`] uses, so the two can never deadlock against each other.
#[must_use]
pub(crate) fn may_spend() -> bool {
    if !ARMED.load(Ordering::Relaxed) {
        return true;
    }
    let billed = usage_totals::snapshot_billed();
    guard()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .is_none_or(|guard| guard.may_spend(&billed))
}

/// The budget block the run summary reports, or `None` when none was set.
#[must_use]
pub(crate) fn outcome() -> Option<Outcome> {
    guard()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .map(|guard| Outcome {
            budget: guard.budget,
            converged: guard.converged,
            stopped: guard.stopped,
        })
}

#[cfg(test)]
mod tests;
