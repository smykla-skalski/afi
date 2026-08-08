//! Automatic context compression: the threshold, and the request that folds.
//!
//! A long run used to end at whichever turn the provider refused for length,
//! with `AFI_AUTOCOMPRESS_PERCENT` describing a fold that never happened. What
//! makes it happen is [`run_autocompress`], called from the turn loop after every
//! turn that reported its token usage: it measures the context the *provider*
//! just counted against the window the source is known to hold, and folds when
//! that crosses the configured percentage.
//!
//! The fold costs one request, and that request goes out through the same
//! non-streaming client call `/compress` uses, so it lands in the run summary's
//! `requests` count and its tokens are billed to the run like any other.

use std::collections::BTreeSet;
use std::sync::{Mutex, PoisonError};

use serde_json::Value;

use super::summary::{self, Summary};
use super::{COMPRESS_KEEP, CompressResult, plan_compression};
use crate::config::{Source, source_prefix};
use crate::model::client::ChatClient;
use crate::model::{TURN_ESC, TurnOutcome};
use crate::term::{MessageKind, UserInterface};

/// What to do about the context after a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decision {
    /// Over the threshold. Carries the window it was measured against, so the
    /// caller does not have to unwrap it a second time to report it.
    Fold { window: u64 },
    /// Leave it alone - under the threshold, or the question does not apply.
    Keep,
    /// Nothing knows this source's window, so the threshold has nothing to
    /// measure against. Distinct from [`Self::Keep`] because it is the one case
    /// worth saying out loud: the operator configured a fold that cannot happen.
    WindowUnknown,
}

/// Whether context usage has crossed the fold threshold.
///
/// [`Decision::Keep`] whenever the question cannot be asked: folding switched off
/// with a percentage of 0, a turn the provider reported no prompt tokens for, or
/// a window declared as 0, which is how an operator turns folding off for one
/// source. A threshold has to measure against something, and inventing the
/// something is how a run folds a conversation that had plenty of room left.
#[must_use]
pub(crate) fn decide(
    prompt_tokens: u64,
    autocompress_percent: u32,
    context_window: Option<u64>,
) -> Decision {
    if autocompress_percent == 0 || prompt_tokens == 0 {
        return Decision::Keep;
    }
    let Some(window) = context_window else {
        return Decision::WindowUnknown;
    };
    // Integer math avoids `u64` -> `f64` precision loss: tokens/window*100 >= pct
    // is equivalent to tokens*100 >= pct*window (`u128` guards multiplication).
    let over =
        u128::from(prompt_tokens) * 100 >= u128::from(autocompress_percent) * u128::from(window);
    if window > 0 && over {
        Decision::Fold { window }
    } else {
        Decision::Keep
    }
}

/// What a fold needs beyond the conversation itself: where to send the summary
/// request, and what to measure the threshold against.
pub(crate) struct AutoCompress<'a> {
    pub client: &'a dyn ChatClient,
    pub source: &'a Source,
    pub model: &'a str,
    /// The configured threshold, as a percentage of `context_window`.
    pub percent: u32,
    /// The window the active source is known to hold, `None` when nothing knows.
    pub context_window: Option<u64>,
}

/// How a fold attempt ended, for a caller deciding whether to try again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fold {
    /// The conversation was folded.
    Done,
    /// Nothing needed doing - under the threshold, or no window to measure
    /// against. The next turn should ask again, since the answer changes as the
    /// context grows.
    NotNeeded,
    /// A fold was attempted and did not happen: refused, timed out, cancelled, or
    /// answered with nothing. **The caller should stop asking for the rest of the
    /// loop.** The conversation is untouched and still over the threshold, so the
    /// next turn would ask again with a bigger prompt and fail the same way - one
    /// wasted request per turn, up to `AFI_MAX_MODEL_TURNS` of them. Cancelling
    /// counts here too: an operator who pressed Esc meant it.
    Abandoned,
}

/// Fold after a turn, when that turn is one worth measuring.
///
/// The two callers fold at different moments - the turn loop between its own
/// requests, the session after the loop returns - and this is the rule they
/// share. Two turns are never measured: an Esc, because firing a request straight
/// after the user asked for the run to stop is not what they asked for, and a
/// failure, because the size it reports is whatever the refused request carried.
/// Both are re-measured on the next turn that answers.
pub(crate) async fn fold_after_turn(
    messages: &mut Vec<Value>,
    outcome: &TurnOutcome,
    ac: &AutoCompress<'_>,
    ui: &mut dyn UserInterface,
) -> Fold {
    let Some(prompt_tokens) = outcome.prompt_tokens() else {
        return Fold::NotNeeded;
    };
    if outcome.status == TURN_ESC || outcome.is_failure() {
        return Fold::NotNeeded;
    }
    run_autocompress(messages, prompt_tokens, ac, ui).await
}

/// Fold the conversation if this turn crossed the threshold.
///
/// Nothing is changed until the summary is back, so an Esc during the request -
/// or a provider that refuses it - leaves the conversation exactly as the turn
/// left it.
async fn run_autocompress(
    messages: &mut Vec<Value>,
    prompt_tokens: u64,
    ac: &AutoCompress<'_>,
    ui: &mut dyn UserInterface,
) -> Fold {
    let window = match decide(prompt_tokens, ac.percent, ac.context_window) {
        Decision::Fold { window } => window,
        Decision::Keep => return Fold::NotNeeded,
        Decision::WindowUnknown => {
            report_unknown_window(ac, ui);
            return Fold::NotNeeded;
        }
    };
    let Some(plan) = plan_compression(messages, COMPRESS_KEEP, true) else {
        // Over the threshold on a conversation too short to fold: one enormous
        // turn rather than a long history. Nothing to summarize away, and nothing
        // a later turn could summarize away either.
        return Fold::Abandoned;
    };
    let cancel = ui.start_activity("Compressing context");
    let summary = summary::fetch(ac.client, ac.source, ac.model, plan.prompt(), &cancel).await;
    ui.stop_activity();

    match summary {
        Summary::Text(text) => {
            let Some(result) = plan.apply(messages, &text) else {
                // A 200 that carried no summary. Saying so matters because the
                // context is still over the threshold - silence here reads as a
                // fold that worked.
                ui.message(
                    MessageKind::Warning,
                    "auto-compress: the model returned an empty summary; context left as it was"
                        .to_string(),
                );
                return Fold::Abandoned;
            };
            announce(&result, prompt_tokens, window, ui);
            Fold::Done
        }
        Summary::Failed(error) => {
            ui.message(
                MessageKind::Warning,
                format!("auto-compress failed, context left as it was: {error}"),
            );
            Fold::Abandoned
        }
        Summary::Cancelled => {
            ui.message(
                MessageKind::Info,
                "auto-compress cancelled; context left as it was".to_string(),
            );
            Fold::Abandoned
        }
    }
}

/// Say what the fold did. One line, because it happens mid-run without anyone
/// asking for it, and a run that silently rewrites its own history is one nobody
/// can account for afterwards.
fn announce(result: &CompressResult, prompt_tokens: u64, window: u64, ui: &mut dyn UserInterface) {
    // `window` came out of `Decision::Fold`, which only produces one above zero,
    // so the division is safe without a second guard here.
    let used = u128::from(prompt_tokens) * 100 / u128::from(window);
    ui.message(
        MessageKind::Info,
        format!(
            "auto-compressed at {used}% of {window}: {} turns summarized, {} kept",
            result.summarized_n, result.kept_n
        ),
    );
}

/// The sources this run has already said it has no window for.
///
/// Keyed by source rather than being one flag, because the notice names a source
/// and tells the operator to set *that source's* variable. A session that
/// `/source`s from one unknown-window source to another would otherwise be told
/// about the first one only, and setting the variable it named would not make the
/// second one fold. Process-wide for the reason `usage_totals` is: one process is
/// one run, and the point of the notice is that a run says it once.
static NOTICED_UNKNOWN_WINDOW: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// Say once per source that folding is configured but has nothing to measure
/// against.
///
/// Reached only through [`Decision::WindowUnknown`], which is already the case
/// where the run would otherwise be silently uncompressed: the threshold is on,
/// the provider is reporting usage, and nothing knows the window. A window
/// declared as 0 never gets here, because that operator has answered the question.
fn report_unknown_window(ac: &AutoCompress<'_>, ui: &mut dyn UserInterface) {
    // A poisoned lock recovers rather than panicking: failing to suppress a
    // duplicate notice must never take down a run.
    let first_time = NOTICED_UNKNOWN_WINDOW
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(ac.source.name.clone());
    if !first_time {
        return;
    }
    // The variable is named through `source_prefix` rather than spelled again
    // here, so the name the operator is told to set is the one the resolver reads.
    // It is the first spelling that resolver tries, which makes it right for a
    // built-in source as well as a configured one.
    ui.message(
        MessageKind::Info,
        format!(
            "auto-compress is on at {}% but the context window of {:?} on {} is unknown, \
             so this run will not compress - set {}CONTEXT_WINDOW \
             (or pass --context-window) to enable it",
            ac.percent,
            ac.model,
            ac.source.name,
            source_prefix(&ac.source.name),
        ),
    );
}
