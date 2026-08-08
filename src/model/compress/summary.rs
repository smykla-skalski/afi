//! Asking the model for the summary a fold folds into.
//!
//! One sender for both folds, because they differ in what they decide and how they
//! report, not in what they ask: a non-streaming completion carrying the plan's
//! prompt, raced against Esc, with the source's own body keys along for a source that
//! has to name a provider to route to.
//!
//! `/compress` had its own copy, and the copy had drifted somewhere worse than
//! duplication - it sent the instruction sentence with the conversation missing, so
//! the model was asked to summarize a history it had never been shown, and whatever
//! it invented replaced the real one. Nothing caught it because the request and the
//! plan were owned by different modules, and only the plan knows what the prompt is
//! supposed to carry.

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::config::Source;
use crate::model::client::ChatClient;
use crate::model::stream::tags;

/// How long to wait for a summary before giving up on the fold.
///
/// Long, because this is one request over a whole conversation rather than a turn: a
/// local server working through a folded-away hour of transcript is slow in a way a
/// remote one is not. A fold that times out leaves the conversation as it was.
const TIMEOUT_SECS: u64 = 120;

/// How the summary request ended.
///
/// Three outcomes rather than a `Result`, because a cancelled fold is not a failure -
/// the operator pressed Esc, and the conversation is meant to be left as it was
/// without an error to account for.
pub(crate) enum Summary {
    Text(String),
    Failed(String),
    Cancelled,
}

/// Ask the model for the summary, racing the request against Esc.
pub(crate) async fn fetch(
    client: &dyn ChatClient,
    source: &Source,
    model: &str,
    prompt: &str,
    cancel: &CancellationToken,
) -> Summary {
    // Bound to a `let` rather than built inline: the future borrows it, and a
    // temporary would be dropped before the `select!` below awaits.
    let ask = [json!({"role": "user", "content": prompt})];
    let request = client.chat_completions(
        source,
        model,
        &ask,
        TIMEOUT_SECS,
        // The source's own body keys, unwrapped, exactly as the streaming path sends
        // them - a source that has to name a provider to route to needs to name it
        // here too.
        source.extra_body.as_ref(),
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
/// wire protocol answered. And one for both folds - a summary is a summary whether an
/// operator asked for it or the threshold did.
///
/// The strip matters here rather than only on the streaming path: a server with no
/// reasoning field puts deliberation in the message body, and a fold would otherwise
/// replace the conversation with the model thinking about summarizing it. A response
/// that is nothing but reasoning strips to empty, which is a failed summary rather
/// than an empty one worth applying.
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
