use std::collections::HashMap;
use std::fs;
use std::path::Path;

use chrono::NaiveDate;
use tempfile::TempDir;

use super::{Table, cache_path, cached, due, layers, vendored};
use crate::pricing::provider::Provider;

fn day(text: &str) -> NaiveDate {
    text.parse().expect("a date")
}

#[test]
fn the_vendored_table_is_readable() {
    // The compiled-in file is `expect`ed at runtime because there is no sensible
    // answer to a broken one - so this is where a broken one is found instead.
    let table = vendored();
    assert!(
        !table.fetched.is_empty(),
        "the file must say when it was projected"
    );
    day(&table.fetched);
    assert!(
        table.providers.len() >= 10,
        "a projection this small means the script narrowed to nothing"
    );
}

#[test]
fn every_shipped_rate_converts() {
    // `layers` drops an entry it cannot convert, which is right for a cache afi
    // wrote and wrong for a file that shipped: there, a dropped entry is a model
    // that silently stops being priced. Counted rather than assumed.
    let raw: usize = vendored().providers.values().map(HashMap::len).sum();
    let (by_provider, _) = layers(&TempDir::new().unwrap().path().join("nothing-here"));
    let converted: usize = by_provider.values().map(HashMap::len).sum();
    assert_eq!(
        converted, raw,
        "every rate in assets/prices.json must convert; re-run `mise run prices:project`"
    );
}

#[test]
fn the_shipped_table_prices_the_models_afi_registers_by_itself() {
    // The four built-in sources exist so nobody has to configure them, and a cap
    // on one of them is only enforceable if the model it defaults to is priced.
    let (by_provider, _) = layers(&TempDir::new().unwrap().path().join("nothing-here"));
    for (provider, model) in [
        // `builtins::ANTHROPIC_MODEL`, and the same model through the
        // aggregator, which spells its ids its own way.
        (Provider::Anthropic, "claude-sonnet-5"),
        (Provider::OpenRouter, "anthropic/claude-sonnet-5"),
        (Provider::Bedrock, "zai.glm-5"),
    ] {
        assert!(
            by_provider
                .get(&provider)
                .is_some_and(|models| models.contains_key(model)),
            "{provider:?} carries no rate for {model}"
        );
    }
}

#[test]
fn a_cache_older_than_the_shipped_table_is_ignored() {
    // An upgrade ships newer rates than the cache the old binary left behind, so
    // the cache has to earn its place rather than win by existing.
    let home = TempDir::new().unwrap();
    let older = day(&vendored().fetched).pred_opt().expect("a day before");
    write_cache(home.path(), &older.to_string());
    assert!(
        cached(home.path()).is_none(),
        "a stale cache must not shadow the table that shipped"
    );
}

#[test]
fn a_newer_cache_replaces_the_shipped_table_outright() {
    let home = TempDir::new().unwrap();
    let newer = day(&vendored().fetched).succ_opt().expect("a day after");
    write_cache(home.path(), &newer.to_string());
    let (by_provider, fetched) = layers(home.path());
    assert_eq!(fetched, newer.to_string());
    assert_eq!(
        by_provider[&Provider::Anthropic].len(),
        1,
        "the cache is the layer, not a patch on top of the shipped one"
    );
}

#[test]
fn a_cache_afi_cannot_read_costs_the_run_nothing() {
    // It is a file afi wrote. Refusing a run over it would turn a bad cache into
    // a stopped session; falling back to the shipped table turns it into a
    // slightly older figure.
    let home = TempDir::new().unwrap();
    for body in ["not json at all", r#"{"fetched": "2099-01-01"}"#, ""] {
        fs::write(cache_path(home.path()), body).unwrap();
        assert!(cached(home.path()).is_none(), "{body:?}");
        let (by_provider, fetched) = layers(home.path());
        assert_eq!(fetched, vendored().fetched);
        assert!(!by_provider.is_empty());
    }
}

#[test]
fn a_table_projected_before_today_is_due() {
    assert!(due("2026-08-07", "2026-08-08"));
    assert!(!due("2026-08-08", "2026-08-08"));
    assert!(
        due("", "2026-08-08"),
        "nothing on disk is as old as it gets"
    );
}

fn write_cache(home: &Path, fetched: &str) {
    let body = format!(
        r#"{{"fetched": "{fetched}", "providers": {{"anthropic": {{"claude-sonnet-4-6": {{"input": 1, "output": 2}}}}}}}}"#
    );
    fs::write(cache_path(home), body).unwrap();
    // The fixture has to be a table afi can read, or the test proves nothing.
    let _: Table = serde_json::from_str(&fs::read_to_string(cache_path(home)).unwrap())
        .expect("the fixture must parse");
}
