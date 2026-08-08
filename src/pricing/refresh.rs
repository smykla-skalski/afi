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

use tokio::task::spawn_blocking;

use super::provider::ALL;
use super::{catalog, table};
use crate::atomic;
use crate::sessions::afi_home;
use crate::util::{is_off, nonblank};

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
/// otherwise, so an air-gapped setup opts out rather than everyone opting in -
/// which is the opposite default to the other two readers of [`is_off`], and the
/// only thing that differs between them.
fn enabled(raw: Option<&str>) -> bool {
    !nonblank(raw).is_some_and(is_off)
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
    // Bytes rather than `text`: `response.text()` runs a UTF-8 validation pass
    // and allocates a multi-megabyte `String` that `serde_json` then validates
    // again.
    let Ok(body) = response.bytes().await else {
        return;
    };
    // ~20 ms of parsing is CPU, and this task shares a runtime with the SSE
    // stream the run is reading. `spawn_blocking` keeps it off those workers.
    let Ok(Some(projection)) = spawn_blocking(move || catalog.project(&body, &ALL)).await else {
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
