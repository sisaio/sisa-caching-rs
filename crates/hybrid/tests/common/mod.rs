//! Helpers shared by the hybrid tests. Each test binary uses some of them.

#![allow(dead_code)]

use std::fmt::Display;

use sisa_caching_core::{
    cache::{CacheEntry, CacheKey, CacheStore, KeyPrefix},
    clock::{Clock, SystemClock},
};
use sisa_caching_hybrid::{
    backplane::RedisBackplane,
    error::HybridCacheError,
    settings::{BackplaneWorkerSettings, RedisBackplaneSettings},
    store::HybridCacheStore,
};
use sisa_caching_memory::store::InMemoryCacheStore;
use sisa_caching_testing::{Redis, eventually};

/// A hybrid store over an in-memory distributed store, with its backplane on Redis.
pub type Node = HybridCacheStore<InMemoryCacheStore>;

pub fn name(name: &str) -> KeyPrefix {
    KeyPrefix::new(name).unwrap()
}

pub fn key(key: impl Display) -> CacheKey {
    CacheKey::new("user", key).unwrap()
}

pub fn memory(store: &str) -> InMemoryCacheStore {
    InMemoryCacheStore::new(store).unwrap()
}

/// Retries quickly, so tests that break the subscription recover fast.
pub fn fast_retries() -> BackplaneWorkerSettings {
    BackplaneWorkerSettings {
        reconnect_delay_ms: 1,
        max_reconnect_delay_ms: 10,
        ..BackplaneWorkerSettings::default()
    }
}

/// Fast retries, and a heartbeat every 20 ms that is missed after 1 s, so a slow CI runner doesn't
/// miss one that is on time.
pub fn fast_heartbeats() -> BackplaneWorkerSettings {
    BackplaneWorkerSettings {
        heartbeat_interval_ms: 20,
        heartbeat_timeout_ms: 1_000,
        ..fast_retries()
    }
}

/// Starts the store's backplane worker with fast retries, unless it already runs, and waits until
/// it has subscribed.
pub async fn start<D: CacheStore + 'static>(store: HybridCacheStore<D>) -> HybridCacheStore<D> {
    start_with(store, fast_retries()).await
}

/// Starts the store's backplane worker with `settings`, unless it already runs, and waits until it
/// has subscribed.
pub async fn start_with<D: CacheStore + 'static>(
    store: HybridCacheStore<D>,
    settings: BackplaneWorkerSettings,
) -> HybridCacheStore<D> {
    let store = store.with_worker_settings(settings).unwrap();

    store.start_worker();
    eventually("the backplane worker subscribes", || async {
        store.local_reads_enabled()
    })
    .await;

    store
}

/// A backplane on `redis` with its own connections, as in a process of its own.
pub async fn backplane(redis: &Redis) -> RedisBackplane {
    RedisBackplane::connect(redis.url(), RedisBackplaneSettings::default())
        .await
        .unwrap()
}

/// A node with a fresh local layer over `distributed`, started.
pub async fn node(distributed: &InMemoryCacheStore, redis: &Redis) -> Node {
    let local = InMemoryCacheStore::new(distributed.name().as_str()).unwrap();

    start(HybridCacheStore::new(local, distributed.clone(), backplane(redis).await).unwrap()).await
}

pub async fn local_value<D: CacheStore + 'static>(
    node: &HybridCacheStore<D>,
    key: &str,
) -> Option<String> {
    node.local_store().get(key).await.unwrap()
}

/// Publishes `payload` on `store`'s backplane channel, as another process would.
pub async fn publish(redis: &Redis, store: &str, payload: &str) {
    redis::cmd("PUBLISH")
        .arg(format!("{store}:backplane"))
        .arg(payload)
        .query_async::<usize>(&mut redis.connection().await)
        .await
        .unwrap();
}

/// The removal of `key` from `store` by `source`, without a `seq`.
pub fn removal(source: &str, store: &str, key: &str) -> String {
    serde_json::json!({
        "v": 1, "source": source, "store": store, "kind": "key", "target": key,
    })
    .to_string()
}

/// Publishes the removal of `key` from `store` as `source` on the store's channel.
pub async fn publish_removal(redis: &Redis, source: &str, store: &str, key: &str) {
    publish(redis, store, &removal(source, store, key)).await;
}

/// Writes `user:1` = `old` to the distributed store, has `reader` copy it, then overwrites it
/// through `writer` and checks the reader drops its copy and reads the new value.
pub async fn assert_a_write_reaches<D, R>(
    writer: &HybridCacheStore<D>,
    reader: &HybridCacheStore<R>,
) where
    D: CacheStore + 'static,
    R: CacheStore + 'static,
{
    writer
        .distributed_store()
        .set(&key(1), "old".to_owned(), None)
        .await
        .unwrap();
    reader.get::<String>(&key(1)).await.unwrap();
    assert_eq!(local_value(reader, &key(1)).await.as_deref(), Some("old"));

    writer.set(&key(1), "new".to_owned(), None).await.unwrap();

    eventually("the reader drops its copy", || async {
        local_value(reader, &key(1)).await.is_none()
    })
    .await;
    assert_eq!(
        reader.get::<String>(&key(1)).await.unwrap().as_deref(),
        Some("new")
    );
}

/// Every write a hybrid store has, each on `user:1`.
#[derive(Clone, Copy, Debug)]
pub enum Write {
    Set,
    SetEntry,
    Remove,
    RemovePrefix,
    Clear,
}

pub const WRITES: [Write; 5] = [
    Write::Set,
    Write::SetEntry,
    Write::Remove,
    Write::RemovePrefix,
    Write::Clear,
];

impl Write {
    pub async fn run<D: CacheStore + 'static>(
        self,
        node: &HybridCacheStore<D>,
    ) -> Result<(), HybridCacheError> {
        match self {
            Self::Set => node.set(&key(1), "new".to_owned(), None).await,
            Self::SetEntry => {
                let entry = CacheEntry::new("new".to_owned(), SystemClock.now(), None);

                node.set_entry(&key(1), entry).await
            }
            Self::Remove => node.remove(&key(1)).await.map(drop),
            Self::RemovePrefix => node.remove_prefix("user").await.map(drop),
            Self::Clear => node.clear().await,
        }
    }

    /// The value under the key once the write is applied.
    pub fn result(self) -> Option<&'static str> {
        match self {
            Self::Set | Self::SetEntry => Some("new"),
            _ => None,
        }
    }
}
