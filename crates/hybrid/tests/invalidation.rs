//! Several nodes sharing one distributed store and one Redis backplane: how a write on one node
//! reaches the local copies on the others, and what happens when the subscription breaks. The
//! distributed store is a memory store, so a test can break the backplane alone.

mod common;

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use common::{WRITES, Write, backplane, key, local_value, memory, node, publish_removal, start};
use sisa_caching_core::{
    cache::{CacheEntry, CacheError, CacheKey, CacheStore, CacheValue, KeyPrefix},
    locker::Locker,
    settings::CacheSettings,
};
use sisa_caching_hybrid::{
    backplane::RedisBackplane, settings::RedisBackplaneSettings, store::HybridCacheStore,
};
use sisa_caching_memory::store::InMemoryCacheStore;
use sisa_caching_testing::{Redis, eventually};
use tokio::sync::Notify;

#[tokio::test]
async fn every_write_evicts_the_copy_on_other_nodes() {
    let redis = Redis::start().await;

    for write in WRITES {
        let distributed = memory("users");
        let writer = node(&distributed, &redis).await;
        let reader = node(&distributed, &redis).await;

        distributed
            .set(&key(1), "old".to_owned(), None)
            .await
            .unwrap();
        reader.get::<String>(&key(1)).await.unwrap();

        write.run(&writer).await.unwrap();

        eventually(&format!("{write:?}: the reader drops its copy"), || async {
            local_value(&reader, &key(1)).await.is_none()
        })
        .await;
        assert_eq!(
            reader.get::<String>(&key(1)).await.unwrap().as_deref(),
            write.result(),
            "{write:?}"
        );
    }
}

#[tokio::test]
async fn a_prefix_removal_keeps_the_copies_under_other_prefixes() {
    let redis = Redis::start().await;
    let distributed = memory("users");
    let writer = node(&distributed, &redis).await;
    let reader = node(&distributed, &redis).await;
    let team = CacheKey::new("team", "1").unwrap();

    distributed
        .set(&key(1), "ada".to_owned(), None)
        .await
        .unwrap();
    distributed
        .set(&team, "core".to_owned(), None)
        .await
        .unwrap();
    reader.get::<String>(&key(1)).await.unwrap();
    reader.get::<String>(&team).await.unwrap();

    writer.remove_prefix("user").await.unwrap();

    eventually("the reader drops the prefix", || async {
        local_value(&reader, &key(1)).await.is_none()
    })
    .await;
    assert_eq!(local_value(&reader, &team).await.as_deref(), Some("core"));
}

#[tokio::test]
async fn a_node_skips_its_own_invalidations_and_other_stores() {
    let redis = Redis::start().await;
    let node = node(&memory("users"), &redis).await;

    node.set(&key(1), "ada".to_owned(), None).await.unwrap();
    node.get::<String>(&key(1)).await.unwrap();
    node.local_store()
        .set(&key("marker"), "x".to_owned(), None)
        .await
        .unwrap();

    publish_removal(&redis, &node.node_id().to_string(), "users", &key(1)).await;
    publish_removal(&redis, "elsewhere", "teams", &key(1)).await;
    // Messages arrive in order: once this one is applied, the node's own message and the other
    // store's have been handled too.
    publish_removal(&redis, "elsewhere", "users", &key("marker")).await;
    eventually("the node catches up", || async {
        local_value(&node, &key("marker")).await.is_none()
    })
    .await;

    assert_eq!(local_value(&node, &key(1)).await.as_deref(), Some("ada"));
}

#[tokio::test]
async fn a_failed_publish_is_tried_again_then_given_up() {
    let redis = Redis::start().await;
    let Some(writer_url) = redis.add_user("writer").await else {
        return;
    };
    let distributed = memory("users");
    let writer = start(
        HybridCacheStore::new(
            memory("users"),
            distributed.clone(),
            RedisBackplane::connect(writer_url, RedisBackplaneSettings::default())
                .await
                .unwrap(),
        )
        .unwrap(),
    )
    .await;
    let reader = node(&distributed, &redis).await;

    distributed
        .set(&key(1), "old".to_owned(), None)
        .await
        .unwrap();
    reader.get::<String>(&key(1)).await.unwrap();

    // The first try is refused; the next comes 50 ms later.
    redis.set_user("writer", "-publish").await;
    writer.set(&key(1), "new".to_owned(), None).await.unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    redis.set_user("writer", "+publish").await;

    eventually("a later try reaches the reader", || async {
        local_value(&reader, &key(1)).await.is_none()
    })
    .await;

    // Every try refused: the write still succeeds, and the reader keeps its copy until it expires.
    reader.get::<String>(&key(1)).await.unwrap();
    reader
        .local_store()
        .set(&key("marker"), "x".to_owned(), None)
        .await
        .unwrap();
    redis.set_user("writer", "-publish").await;
    writer.set(&key(1), "newer".to_owned(), None).await.unwrap();
    // The three tries are over well within this.
    tokio::time::sleep(Duration::from_millis(500)).await;
    // Messages arrive in order: once this one is applied, nothing else is on its way.
    publish_removal(&redis, "elsewhere", "users", &key("marker")).await;
    eventually("the reader catches up", || async {
        local_value(&reader, &key("marker")).await.is_none()
    })
    .await;

    assert_eq!(local_value(&reader, &key(1)).await.as_deref(), Some("new"));
}

#[tokio::test]
async fn the_first_read_starts_the_worker() {
    let redis = Redis::start().await;
    let store =
        HybridCacheStore::new(memory("users"), memory("users"), backplane(&redis).await).unwrap();

    assert!(!store.local_reads_enabled(), "no worker yet");

    store.get::<String>(&key(1)).await.unwrap();

    eventually("the worker subscribes", || async {
        store.local_reads_enabled()
    })
    .await;
}

#[tokio::test]
async fn the_first_write_starts_the_worker_too() {
    let redis = Redis::start().await;
    let store =
        HybridCacheStore::new(memory("users"), memory("users"), backplane(&redis).await).unwrap();

    store.set(&key(1), "ada".to_owned(), None).await.unwrap();

    eventually("the worker subscribes", || async {
        store.local_reads_enabled()
    })
    .await;
}

#[tokio::test]
async fn a_store_and_its_clones_run_one_worker() {
    let redis = Redis::start().await;
    let store = node(&memory("users"), &redis).await;
    let clone = store.clone();

    clone.start_worker();
    clone.set(&key(1), "ada".to_owned(), None).await.unwrap();
    clone.get::<String>(&key(1)).await.unwrap();
    // Time for a second worker, if there were one, to subscribe.
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert_eq!(redis.subscriptions().await, 1);
}

#[tokio::test]
async fn the_worker_runs_until_the_last_clone_drops() {
    let redis = Redis::start().await;
    let store = node(&memory("users"), &redis).await;
    let clone = store.clone();

    drop(store);
    // Lets a worker that was told to stop run its stop.
    tokio::task::yield_now().await;

    assert!(clone.local_reads_enabled(), "the clone keeps it running");

    drop(clone);
    eventually("the worker stops", || async {
        redis.subscriptions().await == 0
    })
    .await;
}

#[tokio::test]
async fn local_reads_go_off_when_the_runtime_running_the_worker_shuts_down() {
    let redis = Redis::start().await;
    // The store's first call starts its worker on a runtime of its own, which shuts down once the
    // node is up.
    let store = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                tokio::runtime::Runtime::new()
                    .unwrap()
                    .block_on(node(&memory("users"), &redis))
            })
            .join()
            .unwrap()
    });

    assert!(!store.local_reads_enabled());
}

#[tokio::test]
async fn a_lost_subscription_reads_through_until_it_is_back() {
    let redis = Redis::start_restartable().await;
    let distributed = memory("users");
    let node = node(&distributed, &redis).await;

    distributed
        .set(&key(1), "ada".to_owned(), None)
        .await
        .unwrap();
    node.get::<String>(&key(1)).await.unwrap();

    redis.stop().await;
    eventually("the node notices", || async { !node.local_reads_enabled() }).await;

    assert_eq!(
        local_value(&node, &key(1)).await,
        None,
        "the loss clears copies"
    );
    assert_eq!(
        node.get::<String>(&key(1)).await.unwrap().as_deref(),
        Some("ada")
    );
    assert_eq!(
        local_value(&node, &key(1)).await,
        None,
        "nothing is copied meanwhile"
    );

    redis.restart().await;
    eventually("the node subscribes again", || async {
        node.local_reads_enabled()
    })
    .await;
    node.get::<String>(&key(1)).await.unwrap();

    assert_eq!(local_value(&node, &key(1)).await.as_deref(), Some("ada"));
}

/// Pauses its reads after they have read and its writes before they write, while `paused` is
/// set, so a test can run another call in between.
#[derive(Clone)]
struct PausingStore {
    inner: InMemoryCacheStore,
    paused: Arc<AtomicBool>,
    read_done: Arc<Notify>,
    finish_read: Arc<Notify>,
    write_started: Arc<Notify>,
    finish_write: Arc<Notify>,
}

impl PausingStore {
    fn new() -> Self {
        Self {
            inner: memory("users"),
            paused: Arc::default(),
            read_done: Arc::default(),
            finish_read: Arc::default(),
            write_started: Arc::default(),
            finish_write: Arc::default(),
        }
    }

    fn pause(&self, paused: bool) {
        self.paused.store(paused, Ordering::SeqCst);
    }

    async fn before_write(&self) {
        if self.paused.load(Ordering::SeqCst) {
            self.write_started.notify_one();
            self.finish_write.notified().await;
        }
    }
}

impl CacheStore for PausingStore {
    type Error = CacheError;

    fn name(&self) -> &KeyPrefix {
        self.inner.name()
    }

    fn cache_settings(&self) -> &CacheSettings {
        self.inner.cache_settings()
    }

    fn locker(&self) -> &Locker {
        self.inner.locker()
    }

    async fn get_entry<CV: CacheValue>(
        &self,
        key: &str,
    ) -> Result<Option<CacheEntry<CV>>, CacheError> {
        let read = self.inner.get_entry::<CV>(key).await?;

        if self.paused.load(Ordering::SeqCst) {
            self.read_done.notify_one();
            self.finish_read.notified().await;
        }

        Ok(read)
    }

    async fn set_entry<CV: CacheValue>(
        &self,
        key: &str,
        entry: CacheEntry<CV>,
    ) -> Result<(), CacheError> {
        self.before_write().await;
        self.inner.set_entry(key, entry).await
    }

    async fn remove(&self, key: &str) -> Result<bool, CacheError> {
        self.inner.remove(key).await
    }

    async fn remove_prefix(&self, prefix: &str) -> Result<usize, CacheError> {
        self.inner.remove_prefix(prefix).await
    }

    async fn clear(&self) -> Result<(), CacheError> {
        self.inner.clear().await
    }
}

async fn pausing_node(redis: &Redis) -> (HybridCacheStore<PausingStore>, PausingStore) {
    let distributed = PausingStore::new();

    distributed
        .inner
        .set(&key(1), "old".to_owned(), None)
        .await
        .unwrap();

    let node = start(
        HybridCacheStore::new(memory("users"), distributed.clone(), backplane(redis).await)
            .unwrap(),
    )
    .await;

    (node, distributed)
}

#[tokio::test]
async fn a_read_during_a_write_does_not_copy_the_old_value() {
    let redis = Redis::start().await;
    let (node, distributed) = pausing_node(&redis).await;

    distributed.pause(true);

    let writer = tokio::spawn({
        let node = node.clone();
        async move { node.set(&key(1), "new".to_owned(), None).await.unwrap() }
    });

    distributed.write_started.notified().await;

    let reader = tokio::spawn({
        let node = node.clone();
        async move { node.get::<String>(&key(1)).await.unwrap() }
    });

    // The read has the old value; the write finishes before the read returns it.
    distributed.read_done.notified().await;
    distributed.finish_write.notify_one();
    writer.await.unwrap();
    distributed.finish_read.notify_one();
    reader.await.unwrap();
    distributed.pause(false);

    assert_eq!(
        node.get::<String>(&key(1)).await.unwrap().as_deref(),
        Some("new")
    );
}

#[tokio::test]
async fn a_read_during_a_removal_does_not_restore_the_value() {
    let redis = Redis::start().await;

    for removal in [Write::Remove, Write::RemovePrefix, Write::Clear] {
        let (node, distributed) = pausing_node(&redis).await;

        distributed.pause(true);

        let reader = tokio::spawn({
            let node = node.clone();
            async move { node.get::<String>(&key(1)).await.unwrap() }
        });

        distributed.read_done.notified().await;
        distributed.pause(false);
        removal.run(&node).await.unwrap();
        distributed.finish_read.notify_one();
        reader.await.unwrap();

        assert_eq!(local_value(&node, &key(1)).await, None, "{removal:?}");
        assert_eq!(
            node.get::<String>(&key(1)).await.unwrap(),
            None,
            "{removal:?}"
        );
    }
}

#[tokio::test]
async fn an_invalidation_from_elsewhere_during_a_read_drops_its_copy() {
    let redis = Redis::start().await;
    let (node, distributed) = pausing_node(&redis).await;

    node.local_store()
        .set(&key("marker"), "x".to_owned(), None)
        .await
        .unwrap();
    distributed.pause(true);

    let reader = tokio::spawn({
        let node = node.clone();
        async move { node.get::<String>(&key(1)).await.unwrap() }
    });

    distributed.read_done.notified().await;
    // Another node writes and publishes while the read holds the old value.
    distributed
        .inner
        .set(&key(1), "new".to_owned(), None)
        .await
        .unwrap();
    publish_removal(&redis, "elsewhere", "users", &key(1)).await;
    publish_removal(&redis, "elsewhere", "users", &key("marker")).await;
    eventually("the invalidations land", || async {
        local_value(&node, &key("marker")).await.is_none()
    })
    .await;
    distributed.pause(false);
    distributed.finish_read.notify_one();
    reader.await.unwrap();

    assert_eq!(local_value(&node, &key(1)).await, None);
    assert_eq!(
        node.get::<String>(&key(1)).await.unwrap().as_deref(),
        Some("new")
    );
}
