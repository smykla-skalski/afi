//! Delivering the machine-readable run summary once a run has finished.
//!
//! Two destinations, asked for independently: stdout under `--summary json`, and
//! a path under `--summary-file`. Both render the same object, built once, so
//! the file and the pipe can never describe different runs.

use std::collections::HashMap;
use std::time::Duration;

use serde_json::Value;

use crate::config::{Runtime, Source};
use crate::model::usage_totals::{self, ByModel, BySource};
use crate::pricing::Pricing;
use crate::summary::{self, RunError, RunSummary, SourceSpend, final_answer};

use super::all_instructions;
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
    // One read of the accumulator: folded for the counts, priced for the cost,
    // and split for the per-source breakdown. Reading it three times would let
    // the three describe different instants.
    let billed = usage_totals::snapshot_by_source();
    let by_model = usage_totals::by_model(&billed);
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
        cost_usd: priced(rt.pricing.as_ref(), &by_model),
        elapsed_secs: elapsed.as_secs_f64(),
        tools: rt.tool_policy.permitted(),
        // Read off the source rather than the flag, so what is reported is what
        // the requests carried - including a level `EXTRA_BODY` set by hand.
        effort: rt.active_source().and_then(Source::resolved_effort),
        refused_tool_calls: usage_totals::refused_tool_calls(),
        auth: billing_source(&rt.sources, rt.active_source(), &billed).map(Source::run_auth),
        sources: spend_by_source(&rt.sources, rt.pricing.as_ref(), &billed),
        // A run that reaches here resolved its prompt; the refusal path reports
        // `None` instead - see `RunSummary::refused`.
        system_prompt_mode: Some(rt.prompt().mode()),
        system_prompt_file: rt.prompt().file(),
        // The startup walk's files, then whatever the model reached into later. Read
        // at the end of the run for the same reason the token totals are: a subtree
        // file loaded on the last turn still belongs in the report.
        instructions: all_instructions(rt)
            .into_iter()
            .map(|(path, _, _)| path)
            .collect(),
    }
}

/// What each billed source spent, priced on its own.
///
/// Per source *and* per model, because the two questions have different answers:
/// rates belong to the model, and budgets belong to the source. Pricing each
/// source over the models it actually served keeps a switched session's figures
/// right even when both sources ran the same model, or one source ran two.
///
/// Each figure is rounded to the micro-dollar on its own, so entries can sum to
/// a hair under or over the run's `cost_usd`, which rounds once over everything.
/// A source whose model has no rate simply carries no figure, and takes the run
/// total with it - the same rule the flat field already follows - while the
/// other entries keep theirs.
///
/// Takes the two things it reads rather than the whole `Runtime`, so what it
/// depends on is in its signature and a test can hand it either one.
fn spend_by_source<'a>(
    sources: &'a HashMap<String, Source>,
    pricing: Option<&Pricing>,
    billed: &BySource,
) -> Vec<SourceSpend<'a>> {
    billed
        .iter()
        .map(|(source, by_model)| SourceSpend {
            source: source.clone(),
            usage: usage_totals::total(by_model),
            cost_usd: priced(pricing, by_model),
            auth: sources.get(source).map(Source::run_auth),
        })
        .collect()
}

/// What a set of per-model counts cost, or nothing when the caller configured no
/// rates. Shared by the run's figure and each source's, so the two cannot come
/// to price the same tokens by different routes.
fn priced(pricing: Option<&Pricing>, by_model: &ByModel) -> Option<f64> {
    pricing.and_then(|pricing| pricing.run_cost_usd(by_model))
}

/// The source whose credential paid for the run, if exactly one did.
///
/// Not `active_source`. A piped session can `/source` its way onto a second
/// endpoint after spending, and the summary would then attest to a credential
/// that bought nothing - naming a service account for tokens a personal key
/// paid for. `source` and `model` still report where the session ended up;
/// `auth` reports who was billed, and the two differ exactly when a switch
/// happened.
///
/// Two sources that both spent are not attributable to one credential, so
/// neither is reported. Nothing billed at all falls back to the active source:
/// no spend can be misattributed when there was none, and a failed run still
/// shows which credential it tried.
fn billing_source<'a>(
    sources: &'a HashMap<String, Source>,
    active: Option<&'a Source>,
    billed: &BySource,
) -> Option<&'a Source> {
    match billed.as_slice() {
        [] => active,
        [(only, _)] => sources.get(only),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
