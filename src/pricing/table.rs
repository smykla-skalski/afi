//! Where the rates come from before the caller says anything.
//!
//! Three layers, and each exists because the one under it cannot answer alone.
//! The **vendored** file is compiled in, so a cap holds on a machine with no
//! network, on the first run, and behind an air gap. The **cache** is that file
//! refreshed from the published catalogue, because rates move between releases
//! and a table nobody refreshes is a cap that is quietly wrong. **Overrides** sit
//! above both, because a brand-new model, an enterprise endpoint, or a negotiated
//! rate is a thing only the operator knows.
//!
//! The refresh never touches the run that triggers it. It is spawned once the
//! session is up, writes the cache atomically, and is read by the *next* run -
//! so a slow or unreachable catalogue costs a run nothing, and a failed
//! refresh leaves the last good copy standing rather than a half-written file.

use std::collections::HashMap;
use std::fs;
use std::hash::BuildHasher;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use chrono::NaiveDate;
use serde::Deserialize;

use super::RawRates;
use super::provider::Provider;
use crate::util::env_int;

/// The file name under `AFI_HOME`, and the name of the vendored copy.
const CACHE_FILE: &str = "prices.json";

/// The rate table as afi stores it - see `catalog::render`, which writes it.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Table {
    /// The day the rates were projected. Read back so the footer can say when a
    /// figure is old, which is the whole difference between a table that went
    /// stale and one that went stale without telling anyone.
    pub fetched: String,
    pub providers: HashMap<String, HashMap<String, RawRates>>,
}

/// The table compiled into the binary.
const VENDORED: &str = include_str!("../../assets/prices.json");

/// What a build that shipped an unreadable asset gets told. There is no runtime
/// path to it - `the_vendored_table_is_readable` catches it in the test suite.
const BROKEN: &str = "the vendored rate table must parse; run `mise run prices:project`";

/// The compiled-in table.
///
/// Parsed on first use rather than at startup, so `--help` and `--version` -
/// which are answered before anything else - pay nothing for it, and so does a
/// run whose cache wins.
pub(super) fn vendored() -> &'static Table {
    static PARSED: LazyLock<Table> =
        LazyLock::new(|| serde_json::from_str(VENDORED).expect(BROKEN));
    &PARSED
}

/// The compiled-in table's date, without building the table.
///
/// [`cached`] needs one string to decide which layer wins. Reading it off
/// [`vendored`] would build ~675 models to look at a date and then drop them
/// when the cache is newer - and being a `LazyLock`, leave them resident for the
/// life of the process. On a machine the refresh has reached, which is the
/// steady state this layer exists to produce, that is the whole asset parsed for
/// nothing on every run.
fn vendored_fetched() -> &'static str {
    static FETCHED: LazyLock<String> = LazyLock::new(|| {
        /// Everything but the date is skipped rather than deserialized - no
        /// `deny_unknown_fields`, so `providers` is walked and discarded.
        #[derive(Deserialize)]
        struct Stamp {
            fetched: String,
        }
        serde_json::from_str::<Stamp>(VENDORED)
            .expect(BROKEN)
            .fetched
    });
    &FETCHED
}

/// Where a refreshed table is kept.
pub(crate) fn cache_path(home: &Path) -> PathBuf {
    home.join(CACHE_FILE)
}

/// The refreshed table, when there is one that reads and is newer than the one
/// compiled in.
///
/// Every failure is the same answer - use the vendored copy - because none of
/// them is the operator's problem: a cache that will not parse is a file afi
/// wrote, and refusing a run over it would turn a stale rate into a stopped
/// session.
pub(super) fn cached(home: &Path) -> Option<Table> {
    let body = fs::read_to_string(cache_path(home)).ok()?;
    let table: Table = serde_json::from_str(&body).ok()?;
    (table.fetched.as_str() > vendored_fetched()).then_some(table)
}

/// Whether the table is old enough to be worth refreshing.
///
/// Compared as dates rather than as file times: the file's mtime says when afi
/// last wrote it, which is also what a failed refresh would touch, and the
/// question here is how old the *rates* are.
pub(crate) fn due(fetched: &str, today: &str) -> bool {
    fetched < today
}

/// The layer beneath the operator's own rates: the refreshed table when there
/// is a usable one, else the table compiled in.
///
/// An entry whose rates will not convert is dropped rather than fatal. Dropping
/// it leaves that one model unpriced, which reports no figure; refusing the
/// whole table would take every other model's figure down with it, and a
/// corrupted cache is a file afi wrote rather than anything the operator did.
pub(super) fn layers(home: &Path) -> (Providers, String) {
    // Borrowed, not cloned: `layers` reads `providers` and takes only `fetched`,
    // so copying ~690 strings and 15 maps of the compiled-in table to drop them
    // again bought nothing.
    let refreshed = cached(home);
    let table = refreshed.as_ref().unwrap_or_else(|| vendored());
    let by_provider = table
        .providers
        .iter()
        // A cache written by a newer afi can name a provider this one has never
        // heard of. That is a row to skip, not a file to refuse.
        .filter_map(|(key, models)| Some((Provider::from_key(key)?, models)))
        .map(|(provider, models)| (provider, priced(models)))
        .collect();
    (by_provider, table.fetched.clone())
}

/// One provider's models, normalized, with any id that collides dropped.
///
/// Two spellings of one id would otherwise resolve by `HashMap` iteration order,
/// which `RandomState` varies per process - so the bill and the cap would move
/// run to run. That is the single failure `Pricing::normalize` refuses an
/// `AFI_PRICES` duplicate to prevent and `scripts/project-prices.py` exits over;
/// this is the third path and it now refuses too.
///
/// Both entries go rather than one winning: reporting no figure is checkable,
/// and a figure that depends on which spelling survived is not.
fn priced(models: &HashMap<String, RawRates>) -> HashMap<String, super::Rates> {
    let mut out: HashMap<String, super::Rates> = HashMap::with_capacity(models.len());
    let mut collided: Vec<String> = Vec::new();
    for (model, raw) in models {
        let Ok(rates) = raw.to_rates() else { continue };
        let key = super::key(model);
        if out.insert(key.clone(), rates).is_some() {
            collided.push(key);
        }
    }
    for key in collided {
        out.remove(&key);
    }
    out
}

/// Provider, then normalized model id, then that model's rates.
pub(super) type Providers = HashMap<Provider, HashMap<String, super::Rates>>;

/// How old the rates may be before a run is told, in days.
const STALE_DAYS: i64 = 30;

/// Say so, once, when the rates a run is about to bill against are old.
///
/// Silent staleness is the failure this whole layer exists to end. A rate that
/// moved six months ago and a rate that is current produce the same confident
/// figure, and the only difference a reader can see is this line.
///
/// stderr rather than the footer: it is a fact about the run rather than about a
/// turn, and the footer would repeat it on every one. Same channel as the
/// warnings a bad `AFI_PRICES` already prints, and for the same reason - it is
/// heard before the run rather than after.
pub(super) fn warn_if_stale<S: BuildHasher>(
    fetched: &str,
    today: NaiveDate,
    env: &HashMap<String, String, S>,
) {
    let limit = env_int(env, "AFI_PRICE_STALE_DAYS", STALE_DAYS);
    // Zero turns the warning off, matching `AFI_AUTOCOMPRESS_PERCENT`.
    if limit <= 0 {
        return;
    }
    let Ok(projected) = fetched.parse::<NaiveDate>() else {
        return;
    };
    let age = (today - projected).num_days();
    if age > limit {
        // Deliberately not "set AFI_PRICE_REFRESH=1": it is on unless turned
        // off, and the run most likely to see this line is a one-shot, which
        // never starts a refresh at all - so that advice would print forever and
        // never move the date.
        eprintln!(
            "afi: token rates were last projected {fetched} ({age} days ago) and any \
             cost_usd is billed against those. Upgrade afi, run an interactive \
             session to refresh them, or set AFI_PRICES to override"
        );
    }
}

#[cfg(test)]
mod tests;
