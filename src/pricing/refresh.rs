//! Keeping the cached rate table current, without a run ever waiting for it.
//!
//! Rates move between releases, so a table that only ships with the binary goes
//! stale in a way that matters: afi caps what a run may spend by pricing what it
//! used, and a stale rate is a cap that is quietly wrong.
//!
//! Nothing here is on the critical path, and that is a promise rather than an
//! optimisation. The fetch is spawned once the session is up and writes a file
//! the *next* run reads, so the catalogue being slow, unreachable, or wrong
//! costs this run nothing. Every failure is silent for the same reason: a rate
//! table is not a thing a run should stop over, and the last good copy is still
//! there.
//!
//! Nothing here knows which catalogue it is talking to - see [`super::catalog`].

use std::collections::HashMap;
use std::hash::BuildHasher;
use std::path::PathBuf;
use std::time::Duration;

use super::provider::ALL;
use super::{catalog, table};
use crate::atomic;
use crate::sessions::afi_home;
use crate::util::nonblank;

/// How long afi waits for the whole catalogue. Generous, because nothing is
/// blocked on it, and finite, because a socket that never answers would leave
/// the task alive for the length of the session.
const TIMEOUT_SECS: u64 = 60;

/// What the run needs to know before it decides to refresh.
pub(crate) struct Plan {
    home: PathBuf,
    today: String,
}

/// Whether this run should refresh, and what it needs to do it.
///
/// `None` when the operator turned it off, or when the table on disk was
/// already projected today. The date rather than the file's mtime, so a refresh
/// that failed and touched nothing does not read as one that succeeded.
pub(crate) fn plan<S: BuildHasher>(
    env: &HashMap<String, String, S>,
    fetched: &str,
    today: String,
) -> Option<Plan> {
    if !enabled(env.get("AFI_PRICE_REFRESH").map(String::as_str)) {
        return None;
    }
    table::due(fetched, &today).then(|| Plan {
        home: afi_home(env),
        today,
    })
}

/// Whether `AFI_PRICE_REFRESH` leaves the refresh on. On unless it plainly says
/// otherwise, so an air-gapped setup opts out rather than everyone opting in.
fn enabled(raw: Option<&str>) -> bool {
    !matches!(
        nonblank(raw).map(str::to_ascii_lowercase).as_deref(),
        Some("0" | "false" | "no" | "off")
    )
}

/// Fetch the catalogue and write the projection to the cache.
///
/// Returns nothing and reports nothing. The one caller spawns this and never
/// looks; a run that ends first simply leaves the cache as it was.
pub(crate) async fn run(plan: Plan) {
    let catalog = catalog::active();
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_secs(TIMEOUT_SECS))
        .build()
    else {
        return;
    };
    let Ok(response) = client.get(catalog.url()).send().await else {
        return;
    };
    let Ok(body) = response.text().await else {
        return;
    };
    let Some(projection) = catalog.project(&body, &ALL) else {
        return;
    };
    // An empty projection is a catalogue that answered and priced nothing afi
    // asked about. Writing it would replace a working table with an empty one,
    // which is the single worst outcome available here.
    if projection.is_empty() {
        return;
    }
    // Atomic, so a run reading the cache sees the old table or the new one and
    // never the prefix of one still being written.
    let body = catalog::render(&projection, &plan.today);
    let _ = atomic::write(&table::cache_path(&plan.home), body.as_bytes());
}

#[cfg(test)]
mod tests;
