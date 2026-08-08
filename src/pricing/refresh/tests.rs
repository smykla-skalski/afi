use std::collections::HashMap;

use super::{enabled, plan};

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
