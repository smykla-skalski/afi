//! The `thinking` request parameter, and the blocks turning it on makes afi
//! responsible for round-tripping.
//!
//! Two things have to stay in step. The request-side `thinking` object decides
//! whether the model reasons at all; when it does, the API requires every
//! thinking block that accompanied a `tool_use` to come back **verbatim** -
//! text and signature - on the request carrying that tool's result. afi keeps
//! history in `OpenAI` shape, which has nowhere to put a signed block, so each
//! assistant turn that carried thinking keeps the raw Anthropic blocks under
//! [`THINKING_HISTORY_KEY`] beside the `OpenAI` copy the rest of the codebase
//! reads. The translator replays them; the `OpenAI` client strips them before
//! they can reach an endpoint that would reject an unknown message field.
//!
//! Sending `disabled` rather than omitting the key remains the default.
//! Thinking is on by default on Claude Opus 5, Claude Sonnet 5, and Claude
//! Fable 5, and Claude Haiku 4.5 rejects `adaptive` outright, so an explicit
//! `disabled` is the configuration that works across the widest set of models.

use std::borrow::Cow;

use serde_json::{Value, json};

/// Where an assistant turn's raw Anthropic thinking blocks live in the
/// `OpenAI`-shape history.
///
/// Prefixed so it cannot collide with a real `OpenAI` message field, and
/// stripped from every `OpenAI`-protocol request: those endpoints range from
/// ignoring an unknown message key (llama.cpp) to rejecting the request over
/// it.
pub(crate) const THINKING_HISTORY_KEY: &str = "afi_thinking";

/// The one `thinking.type` afi treats specially, because it is the default and
/// the only value that means "do not replay".
const DISABLED: &str = "disabled";

/// Whether the translator replays stored thinking blocks into a request.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Thinking {
    /// The request asks the model to think, so stored blocks are echoed back.
    Replay,
    /// The request turns thinking off. A block the model was never asked to
    /// produce is rejected, so stored blocks are dropped.
    Drop,
}

/// The `thinking` value for a request, or `None` to omit the key entirely.
///
/// Three states, all reachable from a source's `EXTRA_BODY`:
///
/// * absent - `{"type": "disabled"}`, the default.
/// * JSON `null` - omitted. Claude Fable 5 rejects an explicit `disabled` and
///   thinks unconditionally, so omission is the only way to reach it.
/// * an object - sent as written, e.g. `{"type": "adaptive", "display": "summarized"}`.
///
/// Anything else is passed through rather than second-guessed; the API rejects
/// a malformed value with a clearer message than afi could write.
///
/// The default gives way to omission above effort `high`, where `disabled` is
/// no longer accepted - see [`disabled_would_be_rejected`].
pub(super) fn resolve(extra_body: Option<&Value>) -> Option<Value> {
    match extra_body.and_then(|body| body.get("thinking")) {
        None if disabled_would_be_rejected(extra_body) => None,
        None => Some(json!({"type": DISABLED})),
        Some(Value::Null) => None,
        Some(value) => Some(value.clone()),
    }
}

/// True at the effort levels where Claude Opus 5 rejects an explicit
/// `disabled`.
///
/// `disabled` is afi's default because it is the value the widest set of models
/// accepts, which stops being true above `high`: asking for that much effort and
/// then turning thinking off is a contradiction the API refuses. Omitting the
/// key leaves the model at its own default, which is the closest thing to
/// "afi did not ask" - and an `--effort max` run failing every turn over a
/// default the caller never set would be afi's bug, not theirs. Anything
/// explicit in `EXTRA_BODY` still wins, including an explicit `disabled`.
fn disabled_would_be_rejected(extra_body: Option<&Value>) -> bool {
    matches!(
        extra_body
            .and_then(|body| body.get("output_config"))
            .and_then(|config| config.get("effort"))
            .and_then(Value::as_str),
        Some("xhigh" | "max")
    )
}

/// The replay mode implied by a resolved `thinking` value.
pub(super) fn mode(resolved: Option<&Value>) -> Thinking {
    let disabled = resolved
        .and_then(|value| value.get("type"))
        .and_then(Value::as_str)
        == Some(DISABLED);
    if disabled {
        Thinking::Drop
    } else {
        // An omitted key leaves the choice to the model, and every model that
        // accepts omission thinks by default.
        Thinking::Replay
    }
}

/// True when a source leaves thinking off.
///
/// The reasoning-only cut exists for local models that loop in their scratchpad
/// forever. Anthropic's thinking is server-side and already bounded by
/// `max_tokens`, so cutting one of those turns short is a false positive; the
/// turn loop uses this to leave the cut off while thinking is on.
pub(crate) fn thinking_disabled(extra_body: Option<&Value>) -> bool {
    mode(resolve(extra_body).as_ref()) == Thinking::Drop
}

/// The thinking blocks stored on an assistant turn, in the order the model
/// produced them.
///
/// Malformed entries are dropped rather than replayed. Anthropic validates the
/// whole request, so one unusable block fails the turn instead of being
/// ignored, and a block afi cannot vouch for is worth less than the turn it
/// would cost.
pub(super) fn stored_blocks(message: &Value) -> Vec<Value> {
    let Some(blocks) = message.get(THINKING_HISTORY_KEY).and_then(Value::as_array) else {
        return Vec::new();
    };
    blocks
        .iter()
        .filter(|block| is_replayable(block))
        .cloned()
        .collect()
}

/// A block the API will accept back: a signed `thinking` block, or the opaque
/// payload of a `redacted_thinking` one.
fn is_replayable(block: &Value) -> bool {
    let non_empty = |key: &str| {
        block
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|text| !text.is_empty())
    };
    match block.get("type").and_then(Value::as_str) {
        // `display: "omitted"` returns an empty `thinking` string, so only the
        // signature has to carry anything.
        Some("thinking") => {
            block.get("thinking").is_some_and(Value::is_string) && non_empty("signature")
        }
        Some("redacted_thinking") => non_empty("data"),
        _ => false,
    }
}

/// True for a thinking block inside an already-translated content array.
pub(super) fn is_block(block: &Value) -> bool {
    matches!(
        block.get("type").and_then(Value::as_str),
        Some("thinking" | "redacted_thinking")
    )
}

/// Drop [`THINKING_HISTORY_KEY`] from every message.
///
/// Borrows when there is nothing to strip, which is every request in a session
/// that never had thinking on.
pub(crate) fn strip_history(messages: &[Value]) -> Cow<'_, [Value]> {
    let carries_blocks = messages
        .iter()
        .any(|message| message.get(THINKING_HISTORY_KEY).is_some());
    if !carries_blocks {
        return Cow::Borrowed(messages);
    }
    Cow::Owned(messages.iter().map(without_thinking).collect())
}

fn without_thinking(message: &Value) -> Value {
    let mut stripped = message.clone();
    if let Some(object) = stripped.as_object_mut() {
        object.remove(THINKING_HISTORY_KEY);
    }
    stripped
}

#[cfg(test)]
mod tests;
