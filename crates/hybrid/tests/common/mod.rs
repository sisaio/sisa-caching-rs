//! Helpers shared by the hybrid tests. Each test binary uses some of them.

#![allow(dead_code)]

use std::{
    fmt::Display,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use sisa_caching_core::{
    cache::{CacheEntry, CacheKey, CacheStore, KeyPrefix},
    clock::{Clock, SystemClock},
    worker::WorkerHost,
};
use sisa_caching_hybrid::{
    backplane::{Backplane, BackplaneMessage, BackplaneSubscription},
    error::HybridCacheError,
    settings::BackplaneWorkerSettings,
    store::HybridCacheStore,
    worker::BackplaneWorker,
};
use sisa_caching_memory::store::InMemoryCacheStore;
use sisa_caching_testing::eventually;
use tokio::sync::{broadcast, watch};

/// A hybrid store over an in-memory distributed store and backplane.
pub type Node = HybridCacheStore<InMemoryCacheStore, InMemoryBackplane>;

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
    }
}

/// Runs the store's backplane worker on a host that lives for the rest of the test, and waits
/// until it has subscribed.
pub async fn start<D, B>(store: HybridCacheStore<D, B>) -> HybridCacheStore<D, B>
where
    D: CacheStore + 'static,
    B: Backplane,
{
    let workers = Box::leak(Box::new(WorkerHost::new()));

    workers.register(BackplaneWorker::new(&store).with_settings(fast_retries()));
    eventually("the backplane worker subscribes", || async {
        store.local_reads_enabled()
    })
    .await;

    store
}

/// A node with a fresh local layer over `distributed`, started.
pub async fn node(distributed: &InMemoryCacheStore, backplane: &InMemoryBackplane) -> Node {
    let local = InMemoryCacheStore::new(distributed.name().as_str()).unwrap();

    start(HybridCacheStore::new(local, distributed.clone(), backplane.clone()).unwrap()).await
}

pub async fn local_value<D: CacheStore + 'static, B: Backplane>(
    node: &HybridCacheStore<D, B>,
    key: &str,
) -> Option<String> {
    node.local_store().get(key).await.unwrap()
}

/// Writes `user:1` = `old` to the distributed store, has `reader` copy it, then overwrites it
/// through `writer` and checks the reader drops its copy and reads the new value.
pub async fn assert_a_write_reaches<D, B, R, RB>(
    writer: &HybridCacheStore<D, B>,
    reader: &HybridCacheStore<R, RB>,
) where
    D: CacheStore + 'static,
    B: Backplane,
    R: CacheStore + 'static,
    RB: Backplane,
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
    pub async fn run<D: CacheStore + 'static, B: Backplane>(
        self,
        node: &HybridCacheStore<D, B>,
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

/// A backplane inside this process. Clones share their subscribers.
#[derive(Clone)]
pub struct InMemoryBackplane {
    messages: broadcast::Sender<BackplaneMessage>,
    connected: Arc<AtomicBool>,
    /// Bumped to end every open subscription.
    epoch: Arc<watch::Sender<u64>>,
    failing_publishes: Arc<Mutex<usize>>,
}

impl InMemoryBackplane {
    pub fn new() -> Self {
        Self {
            messages: broadcast::channel(1024).0,
            connected: Arc::new(AtomicBool::new(true)),
            epoch: Arc::new(watch::channel(0).0),
            failing_publishes: Arc::default(),
        }
    }

    /// Ends every subscription and refuses new ones, as a broken connection would.
    pub fn disconnect(&self) {
        self.connected.store(false, Ordering::SeqCst);
        self.epoch.send_modify(|epoch| *epoch += 1);
    }

    pub fn reconnect(&self) {
        self.connected.store(true, Ordering::SeqCst);
    }

    /// The next `count` publishes fail.
    pub fn fail_publishes(&self, count: usize) {
        *self.failing_publishes.lock().unwrap() = count;
    }

    /// How many of those failures are still to come.
    pub fn failing_publishes(&self) -> usize {
        *self.failing_publishes.lock().unwrap()
    }
}

pub struct InMemorySubscription {
    messages: broadcast::Receiver<BackplaneMessage>,
    epoch: watch::Receiver<u64>,
}

impl BackplaneSubscription for InMemorySubscription {
    type Error = std::io::Error;

    async fn next(&mut self) -> std::io::Result<BackplaneMessage> {
        tokio::select! {
            message = self.messages.recv() => message.map_err(std::io::Error::other),
            _ = self.epoch.changed() => Err(std::io::Error::other("subscription ended")),
        }
    }
}

impl Backplane for InMemoryBackplane {
    type Error = std::io::Error;
    type Subscription = InMemorySubscription;

    async fn publish(&self, message: &BackplaneMessage) -> std::io::Result<()> {
        {
            let mut failing = self.failing_publishes.lock().unwrap();

            if *failing > 0 {
                *failing -= 1;

                return Err(std::io::Error::other("publish failed"));
            }
        }

        _ = self.messages.send(message.clone());

        Ok(())
    }

    async fn subscribe(&self) -> std::io::Result<InMemorySubscription> {
        if !self.connected.load(Ordering::SeqCst) {
            return Err(std::io::Error::other("disconnected"));
        }

        let mut epoch = self.epoch.subscribe();

        epoch.mark_unchanged();

        Ok(InMemorySubscription {
            messages: self.messages.subscribe(),
            epoch,
        })
    }
}
