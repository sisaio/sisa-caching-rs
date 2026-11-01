//! One hybrid node: what reads and writes do to its local and distributed layers. The distributed
//! layer is a memory store; only the backplane is on Redis.

use std::{
    convert::Infallible,
    future::pending,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use crate::common::{backplane, fast_retries, hybrid, key, local_value, memory, node, start};
use futures_util::FutureExt;
use sisa_caching_core::cache::{CacheError, CacheKey, CacheOptions, CacheStore};
use sisa_caching_hybrid::{
    error::HybridCacheError, settings::HybridCacheSettings, store::HybridCacheStore,
};
use sisa_caching_memory::{settings::InMemoryCacheSettings, store::InMemoryCacheStore};
use sisa_caching_testing::{ManualClock, Redis};

fn secs(secs: u64) -> Duration {
    Duration::from_secs(secs)
}

fn ttl(ttl_secs: u64) -> CacheOptions {
    CacheOptions::with_ttl(secs(ttl_secs))
}

fn on_clock(
    clock: &ManualClock,
    settings: InMemoryCacheSettings,
) -> InMemoryCacheStore<ManualClock> {
    InMemoryCacheStore::new("users")
        .unwrap()
        .with_clock(clock.clone())
        .with_settings(settings)
        .unwrap()
}

/// Fast retries, with local copies kept for `local_ttl_secs`.
fn with_local_ttl(local_ttl_secs: u64) -> HybridCacheSettings {
    let mut settings = fast_retries();
    settings.local.ttl_secs = local_ttl_secs;
    settings
}

#[tokio::test]
async fn a_read_copies_a_distributed_hit_unchanged_for_the_local_ttl() {
    let redis = Redis::start().await;
    let clock = ManualClock::new();
    let distributed = on_clock(&clock, InMemoryCacheSettings::default());
    let node = start(
        HybridCacheStore::new_on_clock(
            distributed.clone(),
            backplane(&redis).await,
            &with_local_ttl(10),
            clock.clone(),
        )
        .unwrap(),
    )
    .await;

    distributed
        .set(&key(1), "ada".to_owned(), ttl(100))
        .await
        .unwrap();

    let original = distributed.get_entry::<String>(&key(1)).await.unwrap();

    assert_eq!(local_value(&node, &key(1)).await, None);
    assert_eq!(
        node.get::<String>(&key(1)).await.unwrap().as_deref(),
        Some("ada")
    );
    assert_eq!(
        node.local_store().get_entry(&key(1)).await.unwrap(),
        original,
        "same value, creation time and TTL"
    );

    clock.advance(secs(10));
    assert_eq!(
        local_value(&node, &key(1)).await,
        None,
        "the local TTL let it go"
    );
    assert_eq!(
        node.get_entry(&key(1)).await.unwrap(),
        original,
        "read again"
    );
    assert_eq!(
        node.local_store().get_entry(&key(1)).await.unwrap(),
        original,
        "copied again"
    );

    clock.advance(secs(90));
    assert_eq!(node.get::<String>(&key(1)).await.unwrap(), None);
}

#[tokio::test]
async fn a_write_updates_the_distributed_layer_and_evicts_the_local_copy() {
    let redis = Redis::start().await;
    let distributed = memory("users");
    let node = node(&distributed, &redis).await;

    node.set(&key(1), "old".to_owned(), None).await.unwrap();
    node.get::<String>(&key(1)).await.unwrap();
    node.set(&key(1), "new".to_owned(), None).await.unwrap();

    assert_eq!(local_value(&node, &key(1)).await, None);
    assert_eq!(
        distributed.get::<String>(&key(1)).await.unwrap().as_deref(),
        Some("new")
    );
}

#[tokio::test]
async fn calls_use_the_hybrid_stores_own_settings() {
    let redis = Redis::start().await;
    let clock = ManualClock::new();
    let distributed = on_clock(&clock, InMemoryCacheSettings::default());
    let settings = {
        let mut settings = fast_retries();
        settings.common.default_ttl_secs = Some(3);
        settings.common.lock_timeout_ms = Some(20);
        settings
    };
    let node = start(
        HybridCacheStore::new_on_clock(
            distributed.clone(),
            backplane(&redis).await,
            &settings,
            clock.clone(),
        )
        .unwrap(),
    )
    .await;

    assert_eq!(node.cache_settings(), &settings.common);

    // A write's own TTL, or the hybrid store's default of 3 seconds.
    node.set(&key(1), "default".to_owned(), None).await.unwrap();
    node.set(&key(2), "own".to_owned(), ttl(1)).await.unwrap();

    clock.advance(secs(2));
    assert!(node.get::<String>(&key(1)).await.unwrap().is_some());
    assert!(node.get::<String>(&key(2)).await.unwrap().is_none());

    clock.advance(secs(1));
    assert!(node.get::<String>(&key(1)).await.unwrap().is_none());

    // The hybrid store's 20 ms lock timeout, not the default 30 s. The first load never
    // finishes, so once polled it holds the key's lock for good.
    let user = key(3);
    let mut stuck = Box::pin(node.get_or_set(&user, pending::<Result<String, Infallible>>, None));

    assert!((&mut stuck).now_or_never().is_none());

    let loaded = tokio::time::timeout(
        secs(1),
        node.get_or_set(&user, async || Ok::<_, Infallible>("ada".to_owned()), None),
    )
    .await
    .expect("the 20 ms lock timeout ends the wait");

    assert_eq!(loaded.as_deref(), Ok("ada"));
}

#[tokio::test]
async fn the_local_ttl_does_not_make_get_or_set_refresh_early() {
    let redis = Redis::start().await;
    let clock = ManualClock::new();
    let settings = {
        let mut settings = with_local_ttl(10);
        settings.common.default_ttl_secs = Some(100);
        settings.common.refresh_threshold = Some(0.8);
        settings
    };
    let node = start(
        HybridCacheStore::new_on_clock(
            on_clock(&clock, InMemoryCacheSettings::default()),
            backplane(&redis).await,
            &settings,
            clock.clone(),
        )
        .unwrap(),
    )
    .await;
    let loads = AtomicUsize::new(0);
    let user = key(1);
    let get_or_set = || {
        node.get_or_set(
            &user,
            async || {
                loads.fetch_add(1, Ordering::SeqCst);

                Ok::<_, Infallible>("new".to_owned())
            },
            None,
        )
    };

    node.set(&key(1), "old".to_owned(), None).await.unwrap();
    node.get::<String>(&key(1)).await.unwrap();

    clock.advance(secs(8));
    assert_eq!(get_or_set().await.as_deref(), Ok("old"), "8 of 100 s");

    clock.advance(secs(72));
    assert_eq!(get_or_set().await.as_deref(), Ok("new"), "80 of 100 s");
    assert_eq!(loads.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn one_store_serves_many_value_types() {
    let redis = Redis::start().await;
    let node = node(&memory("users"), &redis).await;
    let count = CacheKey::new("count", "visits").unwrap();

    node.set(&count, 3_u64, None).await.unwrap();
    node.set(&key(1), "ada".to_owned(), None).await.unwrap();

    assert_eq!(node.get::<u64>(&count).await.unwrap(), Some(3));
    assert_eq!(
        node.get::<String>(&key(1)).await.unwrap().as_deref(),
        Some("ada")
    );
    assert!(
        node.get::<String>(&count).await.is_err(),
        "the wrong type is an error"
    );
    assert_eq!(
        node.local_store().get::<u64>(&count).await.unwrap(),
        Some(3),
        "the local copy keeps the type it was read as"
    );
}

/// A value that counts how many times it was cloned, in all.
#[derive(serde::Serialize, serde::Deserialize)]
struct Counted(String);

static CLONES: AtomicUsize = AtomicUsize::new(0);

// Counts each clone, so a test can tell how many copies a read made.
impl Clone for Counted {
    fn clone(&self) -> Self {
        CLONES.fetch_add(1, Ordering::SeqCst);
        Self(self.0.clone())
    }
}

#[tokio::test]
async fn a_read_with_local_reads_off_makes_no_copy_for_the_local_layer() {
    let redis = Redis::start().await;
    let distributed = memory("users");
    let node = node(&distributed, &redis).await;

    distributed
        .set(&key(1), Counted("ada".to_owned()), None)
        .await
        .unwrap();
    node.shutdown().await;

    let before = CLONES.load(Ordering::SeqCst);

    assert!(node.get::<Counted>(&key(1)).await.unwrap().is_some());
    assert_eq!(
        CLONES.load(Ordering::SeqCst) - before,
        1,
        "only the distributed memory store's own clone"
    );
}

#[tokio::test]
async fn clones_share_both_layers() {
    let redis = Redis::start().await;
    let distributed = memory("users");
    let node = start(hybrid(distributed.clone(), backplane(&redis).await).unwrap()).await;
    let clone = node.clone();

    node.set(&key(1), "ada".to_owned(), None).await.unwrap();
    assert_eq!(
        clone.get::<String>(&key(1)).await.unwrap().as_deref(),
        Some("ada")
    );
    assert_eq!(local_value(&node, &key(1)).await.as_deref(), Some("ada"));

    clone.remove(&key(1)).await.unwrap();
    assert_eq!(local_value(&node, &key(1)).await, None);
    assert_eq!(distributed.get::<String>(&key(1)).await.unwrap(), None);
}

#[tokio::test]
async fn a_local_copy_from_before_the_worker_subscribes_is_cleared() {
    let redis = Redis::start().await;
    let distributed = memory("users");

    distributed
        .set(&key(1), "ada".to_owned(), None)
        .await
        .unwrap();

    let node = hybrid(distributed, backplane(&redis).await).unwrap();

    node.local_store()
        .set(&key(1), "stale".to_owned(), None)
        .await
        .unwrap();

    let node = start(node).await;

    assert_eq!(local_value(&node, &key(1)).await, None);
    assert_eq!(
        node.get::<String>(&key(1)).await.unwrap().as_deref(),
        Some("ada")
    );
}

#[tokio::test]
async fn new_rejects_settings_out_of_range_or_that_it_would_not_read() {
    let redis = Redis::start().await;
    // A distributed store with a TTL of its own, which hybrid calls never read.
    let with_ttl = InMemoryCacheStore::new("users")
        .unwrap()
        .with_settings({
            let mut settings = InMemoryCacheSettings::default();
            settings.common.default_ttl_secs = Some(60);
            settings
        })
        .unwrap();

    assert!(matches!(
        hybrid(with_ttl, backplane(&redis).await),
        Err(HybridCacheError::Cache(CacheError::InvalidSettings {
            field: "distributed_store",
            ..
        }))
    ));

    // Worker settings out of range.
    let settings = {
        let mut settings = HybridCacheSettings::default();
        settings.worker.heartbeat_interval_ms = 0;
        settings
    };

    assert!(matches!(
        HybridCacheStore::new(memory("users"), backplane(&redis).await, &settings),
        Err(HybridCacheError::Cache(CacheError::InvalidSettings {
            field: "heartbeat_interval_ms",
            ..
        }))
    ));
}

#[tokio::test]
async fn debug_shows_the_store_but_never_a_value() {
    let redis = Redis::start().await;
    let node = node(&memory("users"), &redis).await;

    node.set(&key(1), "secret".to_owned(), None).await.unwrap();
    node.get::<String>(&key(1)).await.unwrap();

    let shown = format!("{node:?}");

    assert!(shown.contains(r#"name: KeyPrefix("users")"#), "{shown}");
    assert!(shown.contains("local_reads_enabled: true"), "{shown}");
    assert!(!shown.contains("secret"), "{shown}");
}
