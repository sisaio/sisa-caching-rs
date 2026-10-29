//! Several nodes sharing one distributed store and one in-memory backplane: how a write on one
//! node reaches the local copies on the others, and what happens when the subscription breaks.

mod common;

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use common::{InMemoryBackplane, WRITES, key, local_value, memory, name, node, start};
use sisa_caching_core::{
    cache::{CacheEntry, CacheError, CacheKey, CacheStore, CacheValue, KeyPrefix},
    locker::Locker,
    settings::CacheSettings,
};
use sisa_caching_hybrid::{
    backplane::{Backplane, BackplaneMessage, BackplaneSubscription, Invalidation, NodeId},
    settings::BackplaneWorkerSettings,
    store::HybridCacheStore,
};
use sisa_caching_memory::store::InMemoryCacheStore;
use sisa_caching_testing::eventually;
use tokio::sync::Notify;

/// Publishes `invalidation` for `store` as another node would.
async fn publish_from_elsewhere(
    backplane: &InMemoryBackplane,
    store: &str,
    invalidation: Invalidation,
) {
    let message = BackplaneMessage::new(NodeId::new("elsewhere"), name(store), invalidation);

    backplane.publish(&message).await.unwrap();
}

#[tokio::test]
async fn every_write_evicts_the_copy_on_other_nodes() {
    for write in WRITES {
        let distributed = memory("users");
        let backplane = InMemoryBackplane::new();
        let writer = node(&distributed, &backplane).await;
        let reader = node(&distributed, &backplane).await;

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
    let distributed = memory("users");
    let backplane = InMemoryBackplane::new();
    let writer = node(&distributed, &backplane).await;
    let reader = node(&distributed, &backplane).await;
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
    let distributed = memory("users");
    let backplane = InMemoryBackplane::new();
    let node = node(&distributed, &backplane).await;

    node.set(&key(1), "ada".to_owned(), None).await.unwrap();
    node.get::<String>(&key(1)).await.unwrap();
    node.local_store()
        .set(&key("marker"), "x".to_owned(), None)
        .await
        .unwrap();

    publish_from_elsewhere(&backplane, "teams", Invalidation::Key(key(1).into())).await;
    // Messages arrive in order: once this one is applied, the node's own message and the other
    // store's have been handled too.
    publish_from_elsewhere(&backplane, "users", Invalidation::Key(key("marker").into())).await;
    eventually("the node catches up", || async {
        local_value(&node, &key("marker")).await.is_none()
    })
    .await;

    assert_eq!(local_value(&node, &key(1)).await.as_deref(), Some("ada"));
}

#[tokio::test]
async fn a_failed_publish_is_tried_three_times_then_given_up() {
    let distributed = memory("users");
    let backplane = InMemoryBackplane::new();
    let writer = node(&distributed, &backplane).await;
    let reader = node(&distributed, &backplane).await;

    distributed
        .set(&key(1), "old".to_owned(), None)
        .await
        .unwrap();
    reader.get::<String>(&key(1)).await.unwrap();

    backplane.fail_publishes(2);
    writer.set(&key(1), "new".to_owned(), None).await.unwrap();

    eventually("the third try reaches the reader", || async {
        local_value(&reader, &key(1)).await.is_none()
    })
    .await;

    // Three failures: the write still succeeds, and the reader keeps its copy until it expires.
    reader.get::<String>(&key(1)).await.unwrap();
    reader
        .local_store()
        .set(&key("marker"), "x".to_owned(), None)
        .await
        .unwrap();
    backplane.fail_publishes(3);
    writer.set(&key(1), "newer".to_owned(), None).await.unwrap();
    eventually("the writer gives up", || async {
        backplane.failing_publishes() == 0
    })
    .await;
    // Messages arrive in order: once this one is applied, nothing else is on its way.
    publish_from_elsewhere(&backplane, "users", Invalidation::Key(key("marker").into())).await;
    eventually("the reader catches up", || async {
        local_value(&reader, &key("marker")).await.is_none()
    })
    .await;

    assert_eq!(local_value(&reader, &key(1)).await.as_deref(), Some("new"));
}

#[tokio::test]
async fn the_first_read_starts_the_worker() {
    let store =
        HybridCacheStore::new(memory("users"), memory("users"), InMemoryBackplane::new()).unwrap();

    assert!(!store.local_reads_enabled(), "no worker yet");

    store.get::<String>(&key(1)).await.unwrap();

    eventually("the worker subscribes", || async {
        store.local_reads_enabled()
    })
    .await;
}

#[tokio::test]
async fn the_first_write_starts_the_worker_too() {
    let store =
        HybridCacheStore::new(memory("users"), memory("users"), InMemoryBackplane::new()).unwrap();

    store.set(&key(1), "ada".to_owned(), None).await.unwrap();

    eventually("the worker subscribes", || async {
        store.local_reads_enabled()
    })
    .await;
}

#[tokio::test]
async fn a_store_and_its_clones_run_one_worker() {
    let backplane = InMemoryBackplane::new();
    let store = node(&memory("users"), &backplane).await;
    let clone = store.clone();

    clone.start_worker();
    clone.set(&key(1), "ada".to_owned(), None).await.unwrap();
    clone.get::<String>(&key(1)).await.unwrap();

    assert_eq!(backplane.subscribers(), 1);
}

#[tokio::test]
async fn the_worker_runs_until_the_last_clone_drops() {
    let backplane = InMemoryBackplane::new();
    let store = node(&memory("users"), &backplane).await;
    let clone = store.clone();

    drop(store);
    // Lets a worker that was told to stop run its stop.
    tokio::task::yield_now().await;

    assert!(clone.local_reads_enabled(), "the clone keeps it running");

    drop(clone);
    eventually("the worker stops", || async {
        backplane.subscribers() == 0
    })
    .await;
}

#[tokio::test]
async fn a_lost_subscription_reads_through_until_it_is_back() {
    let distributed = memory("users");
    let backplane = InMemoryBackplane::new();
    let node = node(&distributed, &backplane).await;

    distributed
        .set(&key(1), "ada".to_owned(), None)
        .await
        .unwrap();
    node.get::<String>(&key(1)).await.unwrap();

    backplane.disconnect();
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

    backplane.reconnect();
    eventually("the node subscribes again", || async {
        node.local_reads_enabled()
    })
    .await;
    node.get::<String>(&key(1)).await.unwrap();

    assert_eq!(local_value(&node, &key(1)).await.as_deref(), Some("ada"));
}

#[tokio::test]
async fn a_subscription_that_falls_behind_clears_and_subscribes_again() {
    let backplane = InMemoryBackplane::new();
    let node = node(&memory("users"), &backplane).await;

    node.local_store()
        .set(&key(1), "ada".to_owned(), None)
        .await
        .unwrap();

    // Publishing never yields, so the worker falls behind the 1024-message buffer.
    for _ in 0..1025 {
        publish_from_elsewhere(&backplane, "users", Invalidation::Key(key("other").into())).await;
    }

    eventually("the lag clears the copies", || async {
        local_value(&node, &key(1)).await.is_none()
    })
    .await;
    eventually("the worker subscribes again", || async {
        node.local_reads_enabled()
    })
    .await;
}

/// A backplane whose subscriptions live as `lifetimes` says, in order, and then stay up. It
/// records when each one opens.
#[derive(Clone, Default)]
struct BreakingBackplane {
    lifetimes: Arc<Mutex<Vec<Duration>>>,
    opened: Arc<Mutex<Vec<tokio::time::Instant>>>,
}

struct BreakingSubscription(Option<Duration>);

impl BackplaneSubscription for BreakingSubscription {
    type Error = std::io::Error;

    async fn next(&mut self) -> std::io::Result<BackplaneMessage> {
        match self.0 {
            Some(lifetime) => tokio::time::sleep(lifetime).await,
            None => std::future::pending().await,
        }

        Err(std::io::Error::other("subscription broke"))
    }
}

impl Backplane for BreakingBackplane {
    type Error = std::io::Error;
    type Subscription = BreakingSubscription;

    async fn publish(&self, _: &BackplaneMessage) -> std::io::Result<()> {
        Ok(())
    }

    async fn subscribe(&self) -> std::io::Result<BreakingSubscription> {
        let mut opened = self.opened.lock().unwrap();
        let lifetimes = self.lifetimes.lock().unwrap();

        opened.push(tokio::time::Instant::now());

        Ok(BreakingSubscription(
            lifetimes.get(opened.len() - 1).copied(),
        ))
    }
}

/// Runs a backplane worker with a 1 ms first wait and a 50 ms cap until `subscribes` subscriptions
/// have opened, and returns the waits between them.
async fn waits_between_subscribes(lifetimes: Vec<Duration>, subscribes: usize) -> Vec<Duration> {
    let backplane = BreakingBackplane::default();

    *backplane.lifetimes.lock().unwrap() = lifetimes.clone();

    let store = HybridCacheStore::new(memory("users"), memory("users"), backplane.clone())
        .unwrap()
        .with_worker_settings(BackplaneWorkerSettings {
            reconnect_delay_ms: 1,
            max_reconnect_delay_ms: 50,
        });

    store.start_worker();
    eventually("the subscriptions open", || async {
        backplane.opened.lock().unwrap().len() >= subscribes
    })
    .await;
    drop(store);

    let opened = backplane.opened.lock().unwrap();

    opened
        .windows(2)
        .zip(&lifetimes)
        .map(|(pair, lifetime)| pair[1] - pair[0] - *lifetime)
        .collect()
}

#[tokio::test(start_paused = true)]
async fn a_subscription_that_keeps_breaking_waits_longer_each_time() {
    let waits = waits_between_subscribes(vec![Duration::ZERO; 8], 9).await;
    let millis: Vec<u128> = waits.iter().map(Duration::as_millis).collect();

    assert_eq!(millis, [1, 2, 4, 8, 16, 32, 50, 50]);
}

#[tokio::test(start_paused = true)]
async fn a_subscription_that_stayed_up_starts_the_wait_over() {
    let mut lifetimes = vec![Duration::ZERO; 6];

    // Six quick breaks raise the wait to the 50 ms cap; the seventh stayed up past the cap.
    lifetimes.push(Duration::from_millis(80));

    let waits = waits_between_subscribes(lifetimes, 8).await;

    assert_eq!(waits.last().unwrap().as_millis(), 1);
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

async fn pausing_node(
    backplane: &InMemoryBackplane,
) -> (
    HybridCacheStore<PausingStore, InMemoryBackplane>,
    PausingStore,
) {
    let distributed = PausingStore::new();

    distributed
        .inner
        .set(&key(1), "old".to_owned(), None)
        .await
        .unwrap();

    let node = start(
        HybridCacheStore::new(memory("users"), distributed.clone(), backplane.clone()).unwrap(),
    )
    .await;

    (node, distributed)
}

#[tokio::test]
async fn a_read_during_a_write_does_not_copy_the_old_value() {
    let (node, distributed) = pausing_node(&InMemoryBackplane::new()).await;

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
    let removals = [
        Invalidation::Key(key(1).into()),
        Invalidation::Prefix(name("user")),
        Invalidation::Clear,
    ];

    for removal in removals {
        let (node, distributed) = pausing_node(&InMemoryBackplane::new()).await;

        distributed.pause(true);

        let reader = tokio::spawn({
            let node = node.clone();
            async move { node.get::<String>(&key(1)).await.unwrap() }
        });

        distributed.read_done.notified().await;
        distributed.pause(false);
        match &removal {
            Invalidation::Key(key) => _ = node.remove(key).await.unwrap(),
            Invalidation::Prefix(prefix) => _ = node.remove_prefix(prefix.as_str()).await.unwrap(),
            Invalidation::Clear => node.clear().await.unwrap(),
            other => unreachable!("{other:?}"),
        }
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
    let backplane = InMemoryBackplane::new();
    let (node, distributed) = pausing_node(&backplane).await;

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
    publish_from_elsewhere(&backplane, "users", Invalidation::Key(key(1).into())).await;
    publish_from_elsewhere(&backplane, "users", Invalidation::Key(key("marker").into())).await;
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
