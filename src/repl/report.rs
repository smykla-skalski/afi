//! Delivering the machine-readable run summary once a run has finished.
//!
//! Two destinations, asked for independently: stdout under `--summary json`, and
//! a path under `--summary-file`. Both render the same object, built once, so
//! the file and the pipe can never describe different runs.

use std::time::Duration;

use serde_json::Value;

use crate::config::{Runtime, Source};
use crate::model::usage_totals;
use crate::summary::{self, RunError, RunSummary, final_answer};
use crate::term::{MessageKind, UserInterface};

/// Report the finished run wherever the caller asked for it. Returns whether the
/// report was delivered.
///
/// A summary file that cannot be written fails the run. Printing it on stdout
/// instead would be no fallback - a caller that named a path is not reading
/// stdout for the answer - and exiting 0 would report a run whose result nobody
/// received as a run that succeeded.
pub(crate) fn report_run(
    rt: &Runtime,
    messages: &[Value],
    error: Option<&RunError>,
    elapsed: Duration,
    ui: &mut dyn UserInterface,
) -> bool {
    let path = rt.summary_file.as_deref();
    if !rt.summary.is_json() && path.is_none() {
        return true;
    }
    let summary = build(rt, messages, error, elapsed).to_json();
    if rt.summary.is_json() {
        println!("{summary}");
    }
    let Some(path) = path else {
        return true;
    };
    match summary::write_file(path, &summary) {
        Ok(()) => true,
        Err(message) => {
            ui.message(MessageKind::Error, message);
            false
        }
    }
}

fn build<'a>(
    rt: &'a Runtime,
    messages: &'a [Value],
    error: Option<&'a RunError>,
    elapsed: Duration,
) -> RunSummary<'a> {
    // One read of the accumulator, folded for the counts and priced for the
    // cost. Reading it twice would let the two describe different instants.
    let by_model = usage_totals::snapshot_by_model();
    RunSummary {
        ok: error.is_none(),
        error: error.map(|error| error.message.as_str()),
        error_kind: error.map(|error| error.kind),
        source: rt.active.as_deref(),
        model: rt.model.as_deref(),
        answer: final_answer(messages),
        usage: usage_totals::total(&by_model),
        // Priced per model, so a session that switched models mid-run is still
        // billed at each one's own rates.
        cost_usd: rt
            .pricing
            .as_ref()
            .and_then(|pricing| pricing.run_cost_usd(&by_model)),
        elapsed_secs: elapsed.as_secs_f64(),
        tools: rt.tool_policy.permitted(),
        // Read off the source rather than the flag, so what is reported is what
        // the requests carried - including a level `EXTRA_BODY` set by hand.
        effort: rt.active_source().and_then(Source::resolved_effort),
    }
}
