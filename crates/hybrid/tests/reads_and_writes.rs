//! One hybrid node: what reads and writes do to its local and distributed layers. The distributed
//! layer is a memory store, so these run without Redis.

mod common;

use std::{
    convert::Infallible,
    future::pending,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use common::{InMemoryBackplane, key, local_value, memory, name, node, start};
use sisa_caching_core::{
    cache::{CacheError, CacheKey, CacheOptions, CacheStore},
    settings::{CacheSettings, MAX_TTL_SECS},
};
use sisa_caching_hybrid::{error::HybridCacheError, store::HybridCacheStore};
use sisa_caching_memory::{settings::InMemoryCacheSettings, store::InMemoryCacheStore};
use sisa_caching_testing::ManualClock;

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

fn with_ttl(ttl_secs: u64) -> InMemoryCacheSettings {
    InMemoryCacheSettings {
        common: CacheSettings {
            ttl_secs: Some(ttl_secs),
            ..CacheSettings::default()
        },
        ..InMemoryCacheSettings::default()
    }
}

fn with_max_ttl(max_ttl_secs: u64) -> InMemoryCacheSettings {
    InMemoryCacheSettings {
        max_ttl_secs: Some(max_ttl_secs),
        ..InMemoryCacheSettings::default()
    }
}

#[tokio::test]
async fn a_read_copies_a_distributed_hit_into_the_local_layer_unchanged() {
    let distributed = memory("users");
    let node = node(&distributed, &InMemoryBackplane::new()).await;

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
    let distributed = memory("users");
    let node = node(&distributed, &InMemoryBackplane::new()).await;

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
async fn a_write_uses_its_own_ttl_or_the_distributed_default() {
    let clock = ManualClock::new();
    let local = on_clock(&clock, InMemoryCacheSettings::default());
    let distributed = on_clock(&clock, with_ttl(3));
    let node =
        start(HybridCacheStore::new(local, distributed, InMemoryBackplane::new()).unwrap()).await;

    node.set(&key(1), "default".to_owned(), None).await.unwrap();
    node.set(&key(2), "own".to_owned(), ttl(1)).await.unwrap();

    clock.advance(secs(2));
    assert!(node.get::<String>(&key(1)).await.unwrap().is_some());
    assert!(node.get::<String>(&key(2)).await.unwrap().is_none());

    clock.advance(secs(1));
    assert!(node.get::<String>(&key(1)).await.unwrap().is_none());
}

#[tokio::test]
async fn a_ttl_over_365_days_fails_before_anything_is_evicted() {
    let node = node(&memory("users"), &InMemoryBackplane::new()).await;

    node.set(&key(1), "ada".to_owned(), None).await.unwrap();
    node.get::<String>(&key(1)).await.unwrap();

    assert!(matches!(
        node.set(&key(1), "bob".to_owned(), ttl(MAX_TTL_SECS + 1))
            .await,
        Err(HybridCacheError::Cache(CacheError::InvalidSettings {
            field: "ttl",
            ..
        }))
    ));
    assert_eq!(local_value(&node, &key(1)).await.as_deref(), Some("ada"));
}

#[tokio::test]
async fn a_local_copy_stays_at_most_max_ttl_and_keeps_the_entrys_metadata() {
    let clock = ManualClock::new();
    let distributed = on_clock(&clock, InMemoryCacheSettings::default());
    let node = start(
        HybridCacheStore::new(
            on_clock(&clock, with_max_ttl(10)),
            distributed.clone(),
            InMemoryBackplane::new(),
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
    let clock = ManualClock::new();
    let distributed = on_clock(
        &clock,
        InMemoryCacheSettings {
            common: CacheSettings {
                ttl_secs: Some(100),
                refresh_threshold: Some(0.8),
                ..CacheSettings::default()
            },
            ..InMemoryCacheSettings::default()
        },
    );
    let node = start(
        HybridCacheStore::new(
            on_clock(&clock, with_max_ttl(10)),
            distributed,
            InMemoryBackplane::new(),
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
    let node = node(&memory("users"), &InMemoryBackplane::new()).await;
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
async fn get_or_set_stores_over_a_value_of_another_type() {
    let distributed = memory("users");
    let node = node(&distributed, &InMemoryBackplane::new()).await;

    node.set(&key(1), 3_u64, None).await.unwrap();

    let loaded = node
        .get_or_set(
            &key(1),
            || async { Ok::<_, Infallible>("ada".to_owned()) },
            None,
        )
        .await;

    assert_eq!(loaded.as_deref(), Ok("ada"));
    assert_eq!(
        distributed.get::<String>(&key(1)).await.unwrap().as_deref(),
        Some("ada")
    );
}

#[tokio::test]
async fn get_or_set_uses_the_distributed_stores_settings() {
    let distributed = InMemoryCacheStore::new("users")
        .unwrap()
        .with_settings(InMemoryCacheSettings {
            common: CacheSettings {
                lock_timeout_ms: Some(20),
                ..CacheSettings::default()
            },
            ..InMemoryCacheSettings::default()
        })
        .unwrap();
    let node = node(&distributed, &InMemoryBackplane::new()).await;
    let user = key(1);
    // The first load never finishes, so it holds the key's lock for good.
    let stuck = node.get_or_set(&user, pending::<Result<String, Infallible>>, None);

    assert_eq!(node.cache_settings(), distributed.cache_settings());

    let loaded = tokio::select! {
        _ = stuck => unreachable!(),
        loaded = node.get_or_set(&user, || async { Ok::<_, Infallible>("ada".to_owned()) }, None) => loaded,
    };

    assert_eq!(
        loaded.as_deref(),
        Ok("ada"),
        "the 20 ms lock timeout ended the wait"
    );
}

#[tokio::test]
async fn clones_share_the_supplied_stores() {
    let local = memory("users");
    let distributed = memory("users");
    let node = start(
        HybridCacheStore::new(local.clone(), distributed.clone(), InMemoryBackplane::new())
            .unwrap(),
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
    let local = memory("users");
    let distributed = memory("users");

    local.set(&key(1), "stale".to_owned(), None).await.unwrap();
    distributed
        .set(&key(1), "ada".to_owned(), None)
        .await
        .unwrap();

    let node =
        start(HybridCacheStore::new(local.clone(), distributed, InMemoryBackplane::new()).unwrap())
            .await;

    assert_eq!(local.get::<String>(&key(1)).await.unwrap(), None);
    assert_eq!(
        node.get::<String>(&key(1)).await.unwrap().as_deref(),
        Some("ada")
    );
}

#[tokio::test]
async fn store_names_must_match_and_a_mismatch_leaves_the_local_store_alone() {
    let local = memory("teams");

    local.set(&key(1), "keep".to_owned(), None).await.unwrap();

    let result = HybridCacheStore::new(local.clone(), memory("users"), InMemoryBackplane::new());

    assert!(matches!(
        result,
        Err(HybridCacheError::StoreNameMismatch { local, distributed })
            if local == name("teams") && distributed == name("users")
    ));
    assert_eq!(
        local.get::<String>(&key(1)).await.unwrap().as_deref(),
        Some("keep")
    );
}

#[tokio::test]
async fn a_local_store_with_common_settings_is_rejected() {
    let local = InMemoryCacheStore::new("users")
        .unwrap()
        .with_settings(InMemoryCacheSettings {
            common: CacheSettings {
                ttl_secs: Some(60),
                ..CacheSettings::default()
            },
            ..InMemoryCacheSettings::default()
        })
        .unwrap();

    assert!(matches!(
        HybridCacheStore::new(local, memory("users"), InMemoryBackplane::new()),
        Err(HybridCacheError::Cache(CacheError::InvalidSettings {
            field: "local.common",
            ..
        }))
    ));
}

#[tokio::test]
async fn debug_shows_the_store_but_never_a_value() {
    let node = node(&memory("users"), &InMemoryBackplane::new()).await;

    node.set(&key(1), "secret".to_owned(), None).await.unwrap();
    node.get::<String>(&key(1)).await.unwrap();

    let shown = format!("{node:?}");

    assert!(shown.contains(r#"name: KeyPrefix("users")"#), "{shown}");
    assert!(shown.contains("local_reads_enabled: true"), "{shown}");
    assert!(!shown.contains("secret"), "{shown}");
}
