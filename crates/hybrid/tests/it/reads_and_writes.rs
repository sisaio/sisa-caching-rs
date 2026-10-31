//! One hybrid node: what reads and writes do to its local and distributed layers. The distributed
//! layer is a memory store; only the backplane is on Redis.

use std::{
    convert::Infallible,
    future::pending,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use crate::common::{backplane, key, local_value, memory, name, node, start};
use futures_util::FutureExt;
use sisa_caching_core::cache::{CacheError, CacheKey, CacheOptions, CacheStore};
use sisa_caching_hybrid::{
    error::HybridCacheError, settings::BackplaneWorkerSettings, store::HybridCacheStore,
};
use sisa_caching_memory::{settings::InMemoryCacheSettings, store::InMemoryCacheStore};
use sisa_caching_testing::{ManualClock, Redis};

fn secs(secs: u64) -> Duration {
    Duration::from_secs(secs)
}

fn ttl(ttl_secs: u64) -> CacheOptions {
    CacheOptions::with_ttl(secs(ttl_secs))
}

fn on_clock(clock: &ManualClock, settings: InMemoryCacheSettings) -> InMemoryCacheStore {
    InMemoryCacheStore::new("users")
        .unwrap()
        .with_clock(clock.clone())
        .with_settings(settings)
        .unwrap()
}

fn with_max_ttl(max_ttl_secs: u64) -> InMemoryCacheSettings {
    let mut settings = InMemoryCacheSettings::default();
    settings.max_ttl_secs = Some(max_ttl_secs);
    settings
}

#[tokio::test]
async fn a_read_copies_a_distributed_hit_into_the_local_layer_unchanged() {
    let redis = Redis::start().await;
    let distributed = memory("users");
    let node = node(&distributed, &redis).await;

    distributed
        .set(&key(1), "ada".to_owned(), ttl(60))
        .await
        .unwrap();

    assert_eq!(local_value(&node, &key(1)).await, None);
    assert_eq!(
        node.get::<String>(&key(1)).await.unwrap().as_deref(),
        Some("ada")
    );
    assert_eq!(
        node.local_store()
            .get_entry::<String>(&key(1))
            .await
            .unwrap(),
        distributed.get_entry(&key(1)).await.unwrap(),
        "same value, creation time and TTL"
    );
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
async fn calls_use_the_distributed_stores_settings() {
    let redis = Redis::start().await;
    let clock = ManualClock::new();
    let local = on_clock(&clock, InMemoryCacheSettings::default());
    let distributed = on_clock(&clock, {
        let mut settings = InMemoryCacheSettings::default();
        settings.common.ttl_secs = Some(3);
        settings.common.lock_timeout_ms = Some(20);
        settings
    });
    let node =
        start(HybridCacheStore::new(local, distributed.clone(), backplane(&redis).await).unwrap())
            .await;

    assert_eq!(node.cache_settings(), distributed.cache_settings());

    // A write's own TTL, or the distributed store's default of 3 seconds.
    node.set(&key(1), "default".to_owned(), None).await.unwrap();
    node.set(&key(2), "own".to_owned(), ttl(1)).await.unwrap();

    clock.advance(secs(2));
    assert!(node.get::<String>(&key(1)).await.unwrap().is_some());
    assert!(node.get::<String>(&key(2)).await.unwrap().is_none());

    clock.advance(secs(1));
    assert!(node.get::<String>(&key(1)).await.unwrap().is_none());

    // The distributed store's 20 ms lock timeout, not the default 30 s. The first load never
    // finishes, so once polled it holds the key's lock for good.
    let user = key(3);
    let mut stuck = Box::pin(node.get_or_set(&user, pending::<Result<String, Infallible>>, None));

    assert!((&mut stuck).now_or_never().is_none());

    let loaded = tokio::time::timeout(
        secs(1),
        node.get_or_set(
            &user,
            || async { Ok::<_, Infallible>("ada".to_owned()) },
            None,
        ),
    )
    .await
    .expect("the 20 ms lock timeout ends the wait");

    assert_eq!(loaded.as_deref(), Ok("ada"));
}

#[tokio::test]
async fn a_local_copy_stays_at_most_max_ttl_and_keeps_the_entrys_metadata() {
    let redis = Redis::start().await;
    let clock = ManualClock::new();
    let distributed = on_clock(&clock, InMemoryCacheSettings::default());
    let node = start(
        HybridCacheStore::new(
            on_clock(&clock, with_max_ttl(10)),
            distributed.clone(),
            backplane(&redis).await,
        )
        .unwrap(),
    )
    .await;

    distributed
        .set(&key(1), "ada".to_owned(), ttl(100))
        .await
        .unwrap();

    let original = distributed.get_entry::<String>(&key(1)).await.unwrap();

    assert_eq!(node.get_entry(&key(1)).await.unwrap(), original);
    assert_eq!(
        node.local_store().get_entry(&key(1)).await.unwrap(),
        original
    );

    clock.advance(secs(10));
    assert_eq!(local_value(&node, &key(1)).await, None, "the cap let it go");
    assert_eq!(
        node.get_entry(&key(1)).await.unwrap(),
        original,
        "read again"
    );

    clock.advance(secs(90));
    assert_eq!(node.get::<String>(&key(1)).await.unwrap(), None);
}

#[tokio::test]
async fn the_local_cap_does_not_make_get_or_set_refresh_early() {
    let redis = Redis::start().await;
    let clock = ManualClock::new();
    let distributed = on_clock(&clock, {
        let mut settings = InMemoryCacheSettings::default();
        settings.common.ttl_secs = Some(100);
        settings.common.refresh_threshold = Some(0.8);
        settings
    });
    let node = start(
        HybridCacheStore::new(
            on_clock(&clock, with_max_ttl(10)),
            distributed,
            backplane(&redis).await,
        )
        .unwrap(),
    )
    .await;
    let loads = AtomicUsize::new(0);
    let user = key(1);
    let get_or_set = || {
        node.get_or_set(
            &user,
            || async {
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

#[tokio::test]
async fn clones_share_the_supplied_stores() {
    let redis = Redis::start().await;
    let local = memory("users");
    let distributed = memory("users");
    let node = start(
        HybridCacheStore::new(local.clone(), distributed.clone(), backplane(&redis).await).unwrap(),
    )
    .await;
    let clone = node.clone();

    node.set(&key(1), "ada".to_owned(), None).await.unwrap();
    assert_eq!(
        clone.get::<String>(&key(1)).await.unwrap().as_deref(),
        Some("ada")
    );
    assert_eq!(
        local.get::<String>(&key(1)).await.unwrap().as_deref(),
        Some("ada")
    );

    clone.remove(&key(1)).await.unwrap();
    assert_eq!(local.get::<String>(&key(1)).await.unwrap(), None);
    assert_eq!(distributed.get::<String>(&key(1)).await.unwrap(), None);
}

#[tokio::test]
async fn the_supplied_local_store_is_cleared_when_the_worker_subscribes() {
    let redis = Redis::start().await;
    let local = memory("users");
    let distributed = memory("users");

    local.set(&key(1), "stale".to_owned(), None).await.unwrap();
    distributed
        .set(&key(1), "ada".to_owned(), None)
        .await
        .unwrap();

    let node =
        start(HybridCacheStore::new(local.clone(), distributed, backplane(&redis).await).unwrap())
            .await;

    assert_eq!(local.get::<String>(&key(1)).await.unwrap(), None);
    assert_eq!(
        node.get::<String>(&key(1)).await.unwrap().as_deref(),
        Some("ada")
    );
}

#[tokio::test]
async fn new_rejects_parts_that_do_not_fit() {
    let redis = Redis::start().await;
    let teams = memory("teams");

    teams.set(&key(1), "keep".to_owned(), None).await.unwrap();

    // Another store name, which leaves the local store alone.
    assert!(matches!(
        HybridCacheStore::new(teams.clone(), memory("users"), backplane(&redis).await),
        Err(HybridCacheError::StoreNameMismatch { local, distributed, .. })
            if local == name("teams") && distributed == name("users")
    ));
    assert_eq!(
        teams.get::<String>(&key(1)).await.unwrap().as_deref(),
        Some("keep")
    );

    // A local store with `common` settings, which hybrid calls never read.
    let with_common = InMemoryCacheStore::new("users")
        .unwrap()
        .with_settings({
            let mut settings = InMemoryCacheSettings::default();
            settings.common.ttl_secs = Some(60);
            settings
        })
        .unwrap();

    assert!(matches!(
        HybridCacheStore::new(with_common, memory("users"), backplane(&redis).await),
        Err(HybridCacheError::Cache(CacheError::InvalidSettings {
            field: "local.common",
            ..
        }))
    ));

    // Worker settings out of range.
    let store =
        HybridCacheStore::new(memory("users"), memory("users"), backplane(&redis).await).unwrap();

    assert!(matches!(
        store.with_worker_settings({
            let mut settings = BackplaneWorkerSettings::default();
            settings.heartbeat_interval_ms = 0;
            settings
        }),
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
