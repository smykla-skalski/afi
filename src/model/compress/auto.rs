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

use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{COMPRESS_KEEP, CompressResult, plan_compression};
use crate::config::Source;
use crate::model::client::ChatClient;
use crate::model::stream::tags;
use crate::term::{MessageKind, UserInterface};

/// How long to wait for a summary before giving up on the fold.
///
/// Longer than the 30 seconds `/compress` allows itself, because this request
/// carries the conversation it is summarizing rather than a bare instruction, and
/// a local server working through a folded-away hour of transcript is slow in a
/// way a remote one is not. A fold that times out costs the run nothing beyond
/// the wait: the conversation is left as it was and the next turn tries again.
const SUMMARY_TIMEOUT_SECS: u64 = 120;

/// Whether context usage has crossed the fold threshold.
///
/// `false` whenever the question cannot be asked: folding switched off with a
/// percentage of 0, a turn the provider reported no prompt tokens for, or a
/// source whose context window nothing knows - including a window declared as 0,
/// which is how an operator turns folding off for one source. A threshold has to
/// measure against something, and inventing the something is how a run folds a
/// conversation that had plenty of room left.
#[must_use]
pub fn maybe_autocompress(
    prompt_tokens: u64,
    autocompress_percent: u32,
    context_window: Option<u64>,
) -> bool {
    if autocompress_percent == 0 || prompt_tokens == 0 {
        return false;
    }
    let Some(max_tokens) = context_window.filter(|window| *window > 0) else {
        return false;
    };
    // Integer math avoids `u64` -> `f64` precision loss: tokens/mx*100 >= pct
    // is equivalent to tokens*100 >= pct*mx (`u128` guards multiplication).
    u128::from(prompt_tokens) * 100 >= u128::from(autocompress_percent) * u128::from(max_tokens)
}

/// What a fold needs beyond the conversation itself: where to send the summary
/// request, and what to measure the threshold against.
pub struct AutoCompress<'a> {
    pub client: &'a dyn ChatClient,
    pub source: &'a Source,
    pub model: &'a str,
    /// The configured threshold, as a percentage of `context_window`.
    pub percent: u32,
    /// The window the active source is known to hold, `None` when nothing knows.
    pub context_window: Option<u64>,
}

/// Fold the conversation if this turn crossed the threshold. Returns whether it
/// did.
///
/// Nothing is changed until the summary is back, so an Esc during the request -
/// or a provider that refuses it - leaves the conversation exactly as the turn
/// left it, and the next turn simply tries again.
pub async fn run_autocompress(
    messages: &mut Vec<Value>,
    prompt_tokens: u64,
    ac: &AutoCompress<'_>,
    ui: &mut dyn UserInterface,
) -> bool {
    if !maybe_autocompress(prompt_tokens, ac.percent, ac.context_window) {
        report_unknown_window(ac, prompt_tokens, ui);
        return false;
    }
    let Some(plan) = plan_compression(messages, COMPRESS_KEEP, true) else {
        // Over the threshold on a conversation too short to fold: one enormous
        // turn rather than a long history. Nothing to summarize away.
        return false;
    };
    let cancel = ui.start_activity("Compressing context");
    let summary = fetch_summary(ac, plan.prompt(), &cancel).await;
    ui.stop_activity();

    match summary {
        Summary::Text(text) => {
            let Some(result) = plan.apply(messages, &text) else {
                // A 200 that carried no summary. Saying so matters because the
                // context is still over the threshold and the next turn will try
                // again - silence here reads as a fold that worked.
                ui.message(
                    MessageKind::Warning,
                    "auto-compress: the model returned an empty summary; context left as it was"
                        .to_string(),
                );
                return false;
            };
            announce(&result, prompt_tokens, ac, ui);
            true
        }
        Summary::Failed(error) => {
            ui.message(
                MessageKind::Warning,
                format!("auto-compress failed, context left as it was: {error}"),
            );
            false
        }
        Summary::Cancelled => {
            ui.message(
                MessageKind::Info,
                "auto-compress cancelled; context left as it was".to_string(),
            );
            false
        }
    }
}

/// How the summary request ended.
enum Summary {
    Text(String),
    Failed(String),
    Cancelled,
}

/// Ask the model for the summary, racing the request against Esc.
async fn fetch_summary(ac: &AutoCompress<'_>, prompt: &str, cancel: &CancellationToken) -> Summary {
    // Bound to a `let` rather than built inline: the future borrows it, and a
    // temporary would be dropped before the `select!` below awaits.
    let ask = [json!({"role": "user", "content": prompt})];
    let request = ac.client.chat_completions(
        ac.source,
        ac.model,
        &ask,
        SUMMARY_TIMEOUT_SECS,
        // The source's own body keys, unwrapped, exactly as the streaming path
        // sends them - a source that has to name a provider to route to needs to
        // name it here too.
        ac.source.extra_body.as_ref(),
    );
    tokio::select! {
        result = request => match result {
            Ok(body) => match completion_content(&body) {
                Some(text) => Summary::Text(text),
                None => Summary::Failed("the response carried no summary".to_string()),
            },
            Err(error) => Summary::Failed(error.to_string()),
        },
        () = cancel.cancelled() => Summary::Cancelled,
    }
}

/// Pull `choices[0].message.content` out of a chat-completions JSON response,
/// stripped of reasoning, with an empty result dropped.
///
/// One parser for both protocols: the Anthropic client reshapes its own response
/// into this form at the boundary, so nothing above the client branches on which
/// wire protocol answered. And one for both folds - a summary is a summary
/// whether an operator asked for it or the threshold did.
///
/// The strip matters here rather than only on the streaming path: a server with
/// no reasoning field puts deliberation in the message body, and a fold would
/// otherwise replace the conversation with the model thinking about summarizing
/// it. A response that is nothing but reasoning strips to empty, which is a
/// failed summary rather than an empty one worth applying.
#[must_use]
pub(crate) fn completion_content(text: &str) -> Option<String> {
    let value: Value = serde_json::from_str(text).ok()?;
    value
        .get("choices")?
        .as_array()?
        .first()?
        .get("message")?
        .get("content")?
        .as_str()
        .map(tags::strip)
        .filter(|summary| !summary.trim().is_empty())
}

/// Say what the fold did. One line, because it happens mid-run without anyone
/// asking for it, and a run that silently rewrites its own history is one nobody
/// can account for afterwards.
fn announce(
    result: &CompressResult,
    prompt_tokens: u64,
    ac: &AutoCompress<'_>,
    ui: &mut dyn UserInterface,
) {
    let used = ac
        .context_window
        .filter(|window| *window > 0)
        .map_or_else(String::new, |window| {
            format!(" at {}% of {window}", percent_used(prompt_tokens, window))
        });
    ui.message(
        MessageKind::Info,
        format!(
            "auto-compressed{used}: {} turns summarized, {} kept",
            result.summarized_n, result.kept_n
        ),
    );
}

/// How full the context was, as a whole percentage. Integer math for the same
/// reason the threshold uses it.
fn percent_used(prompt_tokens: u64, context_window: u64) -> u64 {
    u64::try_from(u128::from(prompt_tokens) * 100 / u128::from(context_window)).unwrap_or(u64::MAX)
}

/// Whether this run has already said it has no window to measure against.
///
/// Process-wide, like `usage_totals`, and for the same reason: one CLI process is
/// one run, and the point of the notice is that it is said once. Per-turn it
/// would be noise, and per-session it would repeat for every message typed into
/// the same REPL.
static NOTICED_UNKNOWN_WINDOW: AtomicBool = AtomicBool::new(false);

/// Say once that folding is configured but has nothing to measure against.
///
/// Only for the case where the run would otherwise be silently uncompressed: the
/// threshold is on, the provider is reporting usage, and the window is unknown. A
/// window declared as 0 says nothing, because that operator has already answered
/// this question.
fn report_unknown_window(ac: &AutoCompress<'_>, prompt_tokens: u64, ui: &mut dyn UserInterface) {
    if ac.percent == 0 || prompt_tokens == 0 || ac.context_window.is_some() {
        return;
    }
    if NOTICED_UNKNOWN_WINDOW.swap(true, Ordering::Relaxed) {
        return;
    }
    // Named for the source rather than generically: `AFI_SOURCE_<NAME>_` is the
    // first spelling the resolver looks at, so it is right for a built-in source
    // as well as a configured one.
    ui.message(
        MessageKind::Info,
        format!(
            "auto-compress is on at {}% but the context window of {:?} on {} is unknown, \
             so this run will not compress - set AFI_SOURCE_{}_CONTEXT_WINDOW \
             (or pass --context-window) to enable it",
            ac.percent,
            ac.model,
            ac.source.name,
            ac.source.name.to_uppercase()
        ),
    );
}
