//! `/compress`: trade the earlier turns for a summary of them.
//!
//! Its own file because it is the one slash command that spends money. Every
//! other `cmd_*` handler rearranges local state; this one issues a billed
//! request, which is why the summary counts it - `requests` counts requests, not
//! turns.
//!
//! It asks the budget nothing. The client it is handed is a
//! [`crate::model::client::Budgeted`], which refuses a billed call once the cap
//! has fired, so the run's bound holds here without this file remembering it -
//! and holds the same way for whatever spends next.

use serde_json::Value;

use super::{Ui, say};
use crate::config::Runtime;
use crate::cost;
use crate::model::client::ReqwestClient;
use crate::model::compress::{self as model_compress, COMPRESS_KEEP, Summary, plan_compression};
use crate::term::MessageKind::{Error, Info, Warning};

pub(super) async fn cmd_compress(
    rt: &Runtime,
    messages: &mut Vec<Value>,
    client: &ReqwestClient,
    ui: Ui<'_>,
) {
    // Through the same plan the automatic fold runs, so `/compress` gets the pieces it
    // was missing by having its own: a prompt that actually carries the conversation,
    // a tail trimmed to something a chat template can render, and the release of the
    // subtree instructions the summarized turns were carrying. `plan_compression`
    // also owns "too short to fold", which this measured itself and got wrong for a
    // history with no system message - it subtracted one regardless.
    let Some(plan) = plan_compression(messages, COMPRESS_KEEP, false) else {
        say(ui, Info, "Nothing to compress (too few turns)");
        return;
    };
    let (Some(source), Some(model)) = (rt.active_source(), rt.model.as_ref()) else {
        say(ui, Error, "No active source");
        return;
    };
    // The one slash command that spends. A run whose cap has already stopped the
    // turn loop must not be able to spend more by typing a command.
    if !cost::may_spend() {
        say(
            ui,
            Warning,
            "COST HARD BUDGET - /compress is a billed request and this run has stopped spending",
        );
        return;
    }

    let cancel = ui.start_activity("Compressing context");
    let summary = model_compress::fetch(client, source, model, plan.prompt(), &cancel).await;
    ui.stop_activity();
    match summary {
        // No `nested::reset()` here, unlike `/reset` - see `nested::reset`. The plan
        // releases what the fold actually dropped.
        Summary::Text(text) => match plan.apply(messages, &text) {
            Some(_) => say(ui, Info, "Compressed context"),
            // An empty summary would replace the conversation and report success.
            None => say(ui, Warning, "Compress produced no summary; kept context"),
        },
        Summary::Failed(error) => say(ui, Error, format!("Compress failed: {error}")),
        Summary::Cancelled => say(ui, Info, "Compression cancelled"),
    }
}
