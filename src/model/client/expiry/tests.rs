//! The staleness rule both federated paths now share.

use super::*;

#[test]
fn a_full_lifetime_is_usable_for_most_of_it() {
    let at = deadline(Duration::from_hours(1));
    assert!(at > Instant::now());
    // The skew comes off the front, so an hour is usable for 59 minutes.
    assert!(at <= Instant::now() + Duration::from_hours(1));
}

/// A lifetime inside the margin must not underflow, and must not be trusted:
/// a request minted at that boundary would race the deadline.
#[test]
fn a_lifetime_shorter_than_the_skew_is_already_stale() {
    for lifetime in [Duration::from_secs(5), Duration::ZERO] {
        assert!(deadline(lifetime) <= Instant::now(), "{lifetime:?}");
    }
}

#[tokio::test]
async fn a_fresh_credential_comes_back_and_a_stale_one_does_not() {
    let cache: Expiring<String> = Expiring::default();
    assert_eq!(cache.fresh("bedrock").await, None, "nothing cached yet");

    cache
        .store(
            "bedrock",
            "live".to_string(),
            deadline(Duration::from_hours(1)),
        )
        .await;
    assert_eq!(cache.fresh("bedrock").await.as_deref(), Some("live"));

    cache
        .store("bedrock", "spent".to_string(), deadline(Duration::ZERO))
        .await;
    assert_eq!(
        cache.fresh("bedrock").await,
        None,
        "past its deadline, so the caller mints a replacement"
    );
}

/// `/source` can switch between two sources on one protocol mid-session, and
/// they may hold different credentials.
#[tokio::test]
async fn each_source_caches_its_own() {
    let cache: Expiring<String> = Expiring::default();
    let hour = || deadline(Duration::from_hours(1));
    cache.store("bedrock", "one".to_string(), hour()).await;
    cache.store("aws", "two".to_string(), hour()).await;
    assert_eq!(cache.fresh("bedrock").await.as_deref(), Some("one"));
    assert_eq!(cache.fresh("aws").await.as_deref(), Some("two"));
}
