use std::collections::{BTreeMap, HashMap};

use super::{enabled, loses_coverage, plan};
use crate::pricing::catalog::Projection;
use crate::pricing::provider::Provider;
use crate::pricing::table::Providers;

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

#[test]
fn the_refresh_is_on_unless_it_is_plainly_turned_off() {
    // On by default, so an ordinary run gets current rates without being asked
    // to opt in, and an air-gapped one opts out with a word it would guess.
    for raw in [
        None,
        Some(""),
        Some("  "),
        Some("1"),
        Some("yes"),
        Some("on"),
    ] {
        assert!(enabled(raw), "{raw:?} must leave the refresh on");
    }
    for raw in ["0", "false", "no", "off", "OFF", "False"] {
        assert!(!enabled(Some(raw)), "{raw} must turn the refresh off");
    }
}

#[test]
fn a_table_projected_today_is_not_fetched_again() {
    // The throttle. Without it every run in a day would pull the whole
    // catalogue to write the file it already had.
    assert!(plan(&env(&[]), "2026-08-08", "2026-08-08".to_string()).is_none());
    assert!(plan(&env(&[]), "2026-08-09", "2026-08-08".to_string()).is_none());
    assert!(plan(&env(&[]), "2026-08-07", "2026-08-08".to_string()).is_some());
    // Nothing on disk at all is as old as it gets.
    assert!(plan(&env(&[]), "", "2026-08-08".to_string()).is_some());
}

#[test]
fn turning_the_refresh_off_beats_being_due() {
    let off = env(&[("AFI_PRICE_REFRESH", "0")]);
    assert!(
        plan(&off, "2020-01-01", "2026-08-08".to_string()).is_none(),
        "an air-gapped setup must make no network call however old its table is"
    );
}

/// A projection carrying exactly these providers, with one priced model each.
fn covering(providers: &[Provider]) -> Projection {
    providers
        .iter()
        .map(|provider| {
            let mut models = BTreeMap::new();
            models.insert(
                "m".to_string(),
                BTreeMap::from([("input", "1".to_string())]),
            );
            (*provider, models)
        })
        .collect()
}

/// The table it would replace, carrying exactly these providers.
fn current(providers: &[Provider]) -> Providers {
    providers
        .iter()
        .map(|provider| (*provider, HashMap::new()))
        .collect()
}

#[test]
fn a_projection_that_lost_a_provider_is_never_written() {
    // The cache outranks the shipped table by date rather than by coverage, and
    // each day's refresh rewrites the same hole with a fresher date - so writing
    // this once takes those rates away for good. A catalogue that renames a
    // provider must cost the run nothing instead.
    let now = current(&[Provider::Anthropic, Provider::Together]);
    assert!(
        loses_coverage(&now, &covering(&[Provider::Anthropic])),
        "dropping Together must refuse the write"
    );
    assert!(
        loses_coverage(&now, &Projection::new()),
        "and an empty projection is the same failure at its worst"
    );
    assert!(!loses_coverage(
        &now,
        &covering(&[Provider::Anthropic, Provider::Together])
    ));
    assert!(
        !loses_coverage(
            &now,
            &covering(&[Provider::Anthropic, Provider::Together, Provider::OpenAi])
        ),
        "a catalogue that gained a provider is welcome"
    );
}
