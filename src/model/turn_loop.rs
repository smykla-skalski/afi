//! The retry loop around a single model turn: it re-runs `model_turn` based on
//! the returned TURN_* status until the turn is DONE or the user escapes, then
//! forces a final answer if the turn budget is exhausted.

use std::collections::HashMap;
use std::path::Path;

use serde_json::{Value, json};

use crate::approval::ApprovalState;
use crate::config::Source;
use crate::cost::{self, Verdict};
use crate::model::client::ChatClient;
use crate::model::compress::{AutoCompress, Fold, fold_after_turn};
use crate::model::recovery::BUDGET_CONVERGE_NUDGE;
use crate::model::turn::{TurnRequest, model_turn};
use crate::model::{
    ModelConfig, TURN_DONE, TURN_EMPTY, TURN_ESC, TURN_FORCE_FINAL, TURN_STREAM_CUT, TURN_TOOL,
    TurnOutcome,
};
use crate::risk::RiskClassifier;
use crate::summary::ErrorKind;
use crate::term::{MessageKind, UserInterface};

/// Bundles the parameters for the model turn loop.
pub struct LoopRequest<'a> {
    pub config: &'a ModelConfig,
    pub client: &'a dyn ChatClient,
    pub source: &'a Source,
    pub model: &'a str,
    pub approval: &'a ApprovalState,
    pub classifier: &'a dyn RiskClassifier,
    pub cwd: &'a Path,
    pub project_root: &'a Path,
    pub env: &'a HashMap<String, String>,
    pub force_final: bool,
    pub recovery_sampling: bool,
}

/// Retry/recovery counters carried across turns of the loop.
struct TurnCounters {
    reasoning_loop_cuts: u32,
    malformed_stream_cuts: u32,
    empty_turn_cuts: u32,
    force_final: bool,
    recovery_sampling: bool,
    /// Set once a fold has been attempted and did not happen, which stops the
    /// loop asking again. See [`Fold::Abandoned`].
    fold_abandoned: bool,
}

/// Build a `TurnRequest` from the loop request and current counters.
fn build_request<'a>(
    lr: &LoopRequest<'a>,
    c: &TurnCounters,
    forced_final: bool,
) -> TurnRequest<'a> {
    TurnRequest {
        config: lr.config,
        client: lr.client,
        source: lr.source,
        model: lr.model,
        approval: lr.approval,
        classifier: lr.classifier,
        cwd: lr.cwd,
        project_root: lr.project_root,
        env: lr.env,
        reasoning_loop_cut_count: c.reasoning_loop_cuts,
        malformed_stream_cut_count: c.malformed_stream_cuts,
        empty_turn_count: c.empty_turn_cuts,
        forced_final,
        recovery_sampling: c.recovery_sampling,
    }
}

/// Update counters based on the returned TURN_* status.
fn transition(status: &str, c: &mut TurnCounters) {
    match status {
        TURN_STREAM_CUT => {
            c.malformed_stream_cuts += 1;
            c.recovery_sampling = true;
        }
        TURN_EMPTY => {
            c.empty_turn_cuts += 1;
            c.recovery_sampling = true;
        }
        TURN_FORCE_FINAL => {
            c.reasoning_loop_cuts += 1;
            c.empty_turn_cuts = 0;
            c.force_final = true;
            c.recovery_sampling = true;
        }
        TURN_TOOL => {
            c.malformed_stream_cuts = 0;
            c.empty_turn_cuts = 0;
        }
        _ => {}
    }
}

/// Fold between this loop's own requests, when the turn that just ended left the
/// context over the configured threshold.
///
/// Only between them: a turn that ends the loop is the *session's* to fold, and
/// whether there is a session at all is something the loop cannot see - see
/// `repl::core::run_user_turn`, which folds after the loop returns, and the
/// one-shot path, which does not because nothing would read the result.
///
/// A fold that is attempted and does not happen stops the loop asking again. The
/// conversation is left untouched and still over the threshold, so without the
/// latch every remaining turn would fire another summary request and fail the
/// same way - and the likeliest reason for failing is that the summary prompt is
/// itself too big, which no later turn improves.
async fn fold_between_turns(
    messages: &mut Vec<Value>,
    outcome: &TurnOutcome,
    lr: &LoopRequest<'_>,
    c: &mut TurnCounters,
    ui: &mut dyn UserInterface,
) {
    if c.fold_abandoned || is_terminal(outcome) {
        return;
    }
    let ac = autocompress_for(lr);
    if fold_after_turn(messages, outcome, &ac, ui).await == Fold::Abandoned {
        c.fold_abandoned = true;
    }
}

/// Whether this turn ends the loop.
fn is_terminal(outcome: &TurnOutcome) -> bool {
    outcome.status == TURN_DONE || outcome.status == TURN_ESC || outcome.is_failure()
}

/// What the fold needs from the loop's request.
fn autocompress_for<'a>(lr: &LoopRequest<'a>) -> AutoCompress<'a> {
    AutoCompress {
        client: lr.client,
        source: lr.source,
        model: lr.model,
        percent: lr.config.autocompress_percent,
        context_window: lr.source.context_window,
    }
}

/// The model turn loop: retries based on TURN_* status until DONE/ESC/FAILED.
///
/// Returns the terminal outcome so a caller can tell a completed run from a failed
/// one - a one-shot run turns that into its exit code, and the run summary reports
/// the failure kind it carries.
pub async fn run_model_turn_loop(
    messages: &mut Vec<Value>,
    lr: LoopRequest<'_>,
    ui: &mut dyn UserInterface,
) -> TurnOutcome {
    let max_turns: u32 = lr
        .env
        .get("AFI_MAX_MODEL_TURNS")
        .and_then(|v| v.parse().ok())
        .unwrap_or(200);
    let mut steps: u32 = 0;
    let mut c = TurnCounters {
        reasoning_loop_cuts: 0,
        malformed_stream_cuts: 0,
        empty_turn_cuts: 0,
        force_final: lr.force_final,
        recovery_sampling: lr.recovery_sampling,
        fold_abandoned: false,
    };

    while steps < max_turns {
        if let Some(outcome) = budget_gate(messages, ui) {
            return outcome;
        }
        let outcome = model_turn(messages, build_request(&lr, &c, c.force_final), ui).await;
        c.force_final = false;
        c.recovery_sampling = false;
        fold_between_turns(messages, &outcome, &lr, &mut c, ui).await;
        // TURN_FAILED is terminal too. Retrying it would hammer a server that
        // just refused us, up to max_turns times.
        if is_terminal(&outcome) {
            return outcome;
        }
        steps += 1;
        transition(outcome.status, &mut c);
    }

    if steps >= max_turns && !c.force_final {
        // The forced final is one more billed request, so the budget answers
        // first. `Soft` has already latched by now, so no second note can exist.
        if let Some(outcome) = budget_gate(messages, ui) {
            return outcome;
        }
        ui.message(
            MessageKind::Warning,
            format!("MODEL TURN LIMIT ({max_turns}) - forcing final"),
        );
        // Terminal, so the fold - if there is a session to fold for - belongs to
        // whoever called this loop.
        return model_turn(messages, build_request(&lr, &c, true), ui).await;
    }
    TurnOutcome::new(TURN_DONE)
}

/// Enforce the run's budget before the request that would spend more.
///
/// `None` means carry on. The soft threshold is one of those: it says something
/// and returns `None`, because the point of a soft threshold is that the run
/// continues.
///
/// Called at the top of the loop body rather than after a turn, because that is
/// the first place the previous turn's spend is visible - `finalize_turn`
/// records into the ledger before `model_turn` returns. afi cannot know what a
/// turn will cost before it runs; what a cap can promise is that the turn
/// *after* the one that crossed never happens.
fn budget_gate(messages: &mut Vec<Value>, ui: &mut dyn UserInterface) -> Option<TurnOutcome> {
    match cost::checkpoint() {
        Verdict::Unlimited | Verdict::Under => None,
        Verdict::Soft(at) => {
            ui.message(
                MessageKind::Warning,
                format!(
                    "COST SOFT BUDGET ({}) - telling the model to converge",
                    at.describe()
                ),
            );
            // Appended rather than folded into the last user turn. That one is
            // several tool calls back, and rewriting it invalidates the cached
            // prefix - so the note announcing the budget would itself be billed
            // a full cache write of the whole history, which on a large context
            // costs more than the gap it is trying to land inside.
            messages.push(json!({
                "role": "user",
                "content": format!("[Runtime note: {BUDGET_CONVERGE_NUDGE}]"),
            }));
            None
        }
        Verdict::Hard(at) => {
            ui.message(
                MessageKind::Warning,
                format!("COST HARD BUDGET ({}) - stopping the run", at.describe()),
            );
            // No forced final. That is one more billed request, and the headroom
            // the hard ratio leaves is a fraction of the cap rather than the
            // price of a turn - at 0.95 on a $1 budget it is $0.05, which does
            // not buy a request against a large context. Whatever the model
            // already said out loud is in `messages`, and `summary::final_answer`
            // walks back to it.
            Some(TurnOutcome::new(TURN_DONE))
        }
        // Not a cap hit: the measurement failed, so the run failed. A budget that
        // cannot be measured must never be treated as no budget.
        Verdict::Unpriceable(why) => Some(TurnOutcome::report(ui, why, ErrorKind::Input)),
    }
}
