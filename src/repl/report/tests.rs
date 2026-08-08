//! What the summary's per-source breakdown is derived from.
//!
//! `spend_by_source` is the only code that turns the ledger into the `sources`
//! array, and everything it gets wrong - crediting the wrong counts, pricing a
//! source at another's rates, naming the credential of whichever source the
//! session ended on - produces a summary that is well-formed and false. The
//! end-to-end test proves the wiring; these prove the derivation, without a
//! process or a socket.

use std::collections::HashMap;

use super::{billing_source, spend_by_source};
use crate::config::{Runtime, Source};
use crate::model::usage_totals::{ByModel, BySource, UsageTotals};
use crate::pricing::Pricing;
use crate::summary::RunAuth;

/// The rates the entries below are priced at, an order of magnitude apart so a
/// source priced at the other's rates cannot land on the right figure.
const RATES: &str = r#"{"model-one": {"input": 1, "output": 2},
                        "model-two": {"input": 10, "output": 20}}"#;

/// A runtime with two sources that authenticate differently, so an entry that
/// took its credential from the wrong source cannot pass for the right one.
///
/// The sources are installed rather than discovered from an environment: what is
/// under test is the derivation, and building it out of `AFI_SOURCE_*` variables
/// would put source discovery and the config schema in the way of it.
///
/// `third` is configured and never billed - the ledger, not the configuration,
/// decides who gets an entry.
fn two_sources() -> Runtime {
    let mut rt = Runtime::build(&["afi".to_string()], HashMap::new(), None);
    rt.sources = [
        source("first", Some("sk-first")),
        source("second", None),
        source("third", Some("sk-third")),
    ]
    .into_iter()
    .map(|source| (source.name.clone(), source))
    .collect();
    // The session ends on `second`, so anything that reads a credential or a
    // count off the active source rather than the billed one names this.
    rt.active = Some("second".to_string());
    rt.pricing = Pricing::parse(Some(RATES));
    rt
}

/// A source holding a static key, or none at all - the llama.cpp case, which
/// reports `NoCredential` rather than the stored placeholder.
fn source(name: &str, api_key: Option<&str>) -> Source {
    Source::new(
        name,
        "http://localhost:8080/v1".to_string(),
        api_key.map(ToString::to_string),
        None,
        None,
        None,
    )
}

fn totals(input: u64, output: u64, requests: u64) -> UsageTotals {
    UsageTotals {
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        reasoning_tokens: 0,
        requests,
    }
}

fn spent(source: &str, model: &str, usage: UsageTotals) -> (String, ByModel) {
    (source.to_string(), vec![(model.to_string(), usage)])
}

/// A million tokens of each kind, so the money below is checkable by eye against
/// the per-million rates in `two_sources`.
const MILLION: u64 = 1_000_000;

#[test]
fn each_entry_takes_its_counts_and_its_credential_from_its_own_source() {
    // The bug this is here to catch: reading either off the active source, which
    // is `second` here, would credit it with the whole run and name its
    // credential for both entries.
    let billed: BySource = vec![
        spent("first", "model-one", totals(MILLION, MILLION, 1)),
        spent("second", "model-two", totals(7, 3, 2)),
    ];
    let rt = two_sources();
    let spend = spend_by_source(&rt, &billed);

    let named: Vec<(&str, u64, u64)> = spend
        .iter()
        .map(|entry| {
            (
                entry.source.as_str(),
                entry.usage.input_tokens,
                entry.usage.requests,
            )
        })
        .collect();
    assert_eq!(
        named,
        vec![("first", MILLION, 1), ("second", 7, 2)],
        "each entry carries what its own source spent"
    );
    // `first` holds a key and `second` holds none, so an entry that read the
    // credential off the wrong source reports the wrong mode.
    assert_eq!(spend[0].auth, Some(RunAuth::ApiKey));
    assert_eq!(spend[1].auth, Some(RunAuth::NoCredential));
}

#[test]
fn each_source_is_priced_at_the_rates_of_the_models_it_served() {
    // Pricing the whole run per entry, or pricing an entry at the other's rates,
    // both produce a plausible figure - so the two sources run models an order of
    // magnitude apart on identical counts.
    let billed: BySource = vec![
        spent("first", "model-one", totals(MILLION, MILLION, 1)),
        spent("second", "model-two", totals(MILLION, MILLION, 1)),
    ];
    let rt = two_sources();
    let spend = spend_by_source(&rt, &billed);
    // 1M x $1 + 1M x $2, and 1M x $10 + 1M x $20.
    assert_eq!(spend[0].cost_usd, Some(3.0));
    assert_eq!(spend[1].cost_usd, Some(30.0));
}

#[test]
fn a_source_that_served_two_models_bills_each_at_its_own_rate() {
    // The reason an entry keeps its per-model split rather than a folded total: a
    // `/source first model-two` switch bills one source at two rates, and pricing
    // the fold at either one would be wrong.
    let billed: BySource = vec![(
        "first".to_string(),
        vec![
            ("model-one".to_string(), totals(MILLION, 0, 1)),
            ("model-two".to_string(), totals(MILLION, 0, 1)),
        ],
    )];
    let rt = two_sources();
    let spend = spend_by_source(&rt, &billed);
    assert_eq!(spend.len(), 1);
    assert_eq!(spend[0].usage.input_tokens, 2 * MILLION, "the counts fold");
    assert_eq!(spend[0].cost_usd, Some(11.0), "the rates do not");
}

#[test]
fn an_unpriced_model_drops_that_entrys_figure_and_leaves_the_others_standing() {
    // Absent beats approximate, per source: a model missing from the table takes
    // its own entry's figure with it and no more.
    let billed: BySource = vec![
        spent("first", "model-one", totals(MILLION, 0, 1)),
        spent("second", "unpriced-model", totals(MILLION, 0, 1)),
    ];
    let rt = two_sources();
    let spend = spend_by_source(&rt, &billed);
    assert_eq!(spend[0].cost_usd, Some(1.0));
    assert_eq!(spend[1].cost_usd, None);
}

#[test]
fn nothing_billed_is_no_entries_at_all() {
    // Not one entry per configured source: three are configured here and none of
    // them spent, and an entry of zeros would read as a source that ran for free.
    let rt = two_sources();
    assert!(spend_by_source(&rt, &BySource::new()).is_empty());
}

#[test]
fn the_run_level_credential_is_the_one_that_paid_when_exactly_one_did() {
    // The rule the breakdown exists beside: one spender is attributable, two are
    // not, and nothing billed falls back to the source the run was pointed at.
    let rt = two_sources();
    let one = vec![spent("first", "model-one", totals(1, 1, 1))];
    assert_eq!(
        billing_source(&rt, &one).map(|source| source.name.clone()),
        Some("first".to_string()),
        "the source that spent, not the active one"
    );

    let two = vec![
        spent("first", "model-one", totals(1, 1, 1)),
        spent("second", "model-two", totals(1, 1, 1)),
    ];
    assert!(billing_source(&rt, &two).is_none(), "no single credential");

    assert_eq!(
        billing_source(&rt, &BySource::new()).map(|source| source.name.clone()),
        Some("second".to_string()),
        "nothing billed reports the credential the run tried"
    );
}
