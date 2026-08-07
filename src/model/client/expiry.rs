//! Credentials that stop working, and the cache that re-mints them first.
//!
//! Both federated paths mint a credential with a lifetime: Anthropic's exchange
//! returns a bearer token good for `expires_in` seconds, AWS's role assumption a
//! key trio good until an `Expiration` instant. Neither is long enough for an
//! interactive session, so both have to be re-minted as they age, and both have
//! to decide the same two things - how close to the deadline still counts as
//! usable, and what to do with a lifetime shorter than that margin.
//!
//! Answered once here. Two copies of this stayed equal only as long as whoever
//! changed one remembered the other, and the failure mode of drifting is a run
//! that dies mid-turn on a credential the other path would have replaced.
//!
//! What is *not* shared is minting: the two exchanges have nothing in common
//! past "post something, get a credential", so each keeps its own.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use tokio::sync::RwLock;

/// Treat a credential as gone this long before it actually expires, so a request
/// that starts near the deadline does not race it.
const EXPIRY_SKEW: Duration = Duration::from_mins(1);

/// A deadline `lifetime` from now, brought forward by [`EXPIRY_SKEW`].
///
/// Saturating rather than checked, so a lifetime shorter than the margin - or
/// zero, which is what an unreadable expiry resolves to - yields a deadline
/// already past instead of underflowing. That credential serves the request it
/// was minted for and is replaced for the next one, which is the safe reading of
/// "afi could not tell how long this lasts".
///
/// A wall-clock lifetime is converted to a monotonic deadline by the caller, so
/// a system-clock step mid-run cannot make a live credential look expired or the
/// reverse.
pub(super) fn deadline(lifetime: Duration) -> Instant {
    Instant::now()
        .checked_add(lifetime.saturating_sub(EXPIRY_SKEW))
        .unwrap_or_else(Instant::now)
}

/// Per-source cache of credentials that expire.
///
/// Keyed by source name because `/source` can switch mid-session between two
/// sources on the same protocol, and they may hold different credentials.
#[derive(Debug)]
pub(super) struct Expiring<T> {
    entries: RwLock<HashMap<String, Cached<T>>>,
}

#[derive(Debug)]
struct Cached<T> {
    value: T,
    expires_at: Instant,
}

/// Hand-written because the derive would demand `T: Default`, which no cached
/// credential has or should have.
impl<T> Default for Expiring<T> {
    fn default() -> Self {
        Self {
            entries: RwLock::default(),
        }
    }
}

impl<T: Clone> Expiring<T> {
    /// The cached credential for `name`, or `None` when there is none or it is
    /// too close to its deadline to use.
    ///
    /// The lock is released before the caller mints a replacement, so a slow
    /// exchange does not block a second source's request. Two first requests
    /// racing therefore both mint, and the loser's is simply overwritten - which
    /// costs one extra exchange and never an incorrect credential.
    pub(super) async fn fresh(&self, name: &str) -> Option<T> {
        let entries = self.entries.read().await;
        let cached = entries.get(name)?;
        (cached.expires_at > Instant::now()).then(|| cached.value.clone())
    }

    /// Cache `value` for `name` until `expires_at`, which [`deadline`] has
    /// already pulled back by the skew.
    pub(super) async fn store(&self, name: &str, value: T, expires_at: Instant) {
        self.entries
            .write()
            .await
            .insert(name.to_string(), Cached { value, expires_at });
    }
}

#[cfg(test)]
mod tests;
