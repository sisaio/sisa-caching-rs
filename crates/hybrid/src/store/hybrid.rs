use std::{sync::Arc, time::Duration};

use redis::IntoConnectionInfo;
use sisa_caching_core::{
    cache::{CacheEntry, CacheError, CacheStore, CacheValue, KeyPrefix},
    clock::{Clock, SystemClock},
    locker::Locker,
    metrics::{Layer, METRICS, Reads},
    settings::CacheSettings,
    worker::Worker,
};
use sisa_caching_memory::store::InMemoryCacheStore;
use sisa_caching_redis::store::RedisCacheStore;
use tracing::Instrument;

use crate::{
    backplane::{BackplaneMessage, Invalidation, NodeId, RedisBackplane},
    error::{HybridCacheError, Result},
    settings::{BackplaneWorkerSettings, HybridCacheSettings},
    worker::BackplaneWorker,
};

use super::{
    RedisHybridCacheStore,
    state::{Publishing, State},
};

/// How many times a write tries to publish its invalidation, waiting 50 ms, then 100 ms, between
/// tries. A late or repeated invalidation only costs a cache miss, so retrying is always safe.
const PUBLISH_ATTEMPTS: u32 = 3;

/// How long `shutdown` waits for the writes still running and their invalidations.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(2);

/// Publishes `message` on `store`'s channel in up to `PUBLISH_ATTEMPTS` tries, and returns how
/// many subscribers it reached.
async fn send(
    backplane: &RedisBackplane,
    store: &KeyPrefix,
    message: &BackplaneMessage,
) -> sisa_caching_redis::error::Result<usize> {
    let mut delay = Duration::from_millis(50);
    let mut attempt = 1;

    loop {
        match backplane.publish(message).await {
            Err(error) if attempt < PUBLISH_ATTEMPTS => {
                tracing::debug!(%store, %error, attempt, "publish failed; retrying");

                tokio::time::sleep(delay).await;
                delay *= 2;
                attempt += 1;
            }
            result => return result,
        }
    }
}

/// The rest of `shutdown`, once heartbeats have stopped: waits for the writes still running, sends
/// the stop message, and stops the worker.
async fn stop<C: Clock>(state: Arc<State<C>>, backplane: Arc<RedisBackplane>, worker: Worker) {
    let store = state.local_store.name();
    let sequence = &state.sequence;
    let published = tokio::time::timeout(SHUTDOWN_WAIT, async {
        while !sequence.idle() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;

    if published.is_err() {
        tracing::warn!(%store, "invalidations were still being published at shutdown; other nodes may clear once");
    }

    let stopping = BackplaneMessage::accounting(
        state.node_id.clone(),
        sequence.last(),
        Invalidation::Stopping,
    );

    match send(&backplane, store, &stopping).await {
        Ok(_) => tracing::info!(%store, "told the other nodes this one is stopping"),
        Err(error) => {
            tracing::warn!(%store, %error, "could not publish the stop; other nodes clear once its heartbeats stop")
        }
    }

    // Only now: until here the worker keeps the local copies current, and on Garnet a
    // subscription closing while a message is published can lose that message for the others.
    worker.stop();
    state.disable_local_reads();
    _ = state.shut_down.set(());
}

/// Publishes one invalidation under its sequence number, retrying with a growing wait. When every
/// try fails, the other nodes see the number missing at this node's next heartbeat and clear their
/// local layers.
async fn publish(
    publishing: Publishing,
    backplane: Arc<RedisBackplane>,
    store: KeyPrefix,
    mut message: BackplaneMessage,
) {
    message.seq = Some(publishing.seq);

    match send(&backplane, &store, &message).await {
        Ok(subscribers) => METRICS.invalidation_delivered(&store, subscribers),
        Err(error) => {
            METRICS.publish_failed(&store);
            tracing::warn!(
                %store,
                %error,
                kind = message.invalidation.kind(),
                "could not publish an invalidation; other nodes clear their local layers at the next heartbeat"
            );
        }
    }
}

/// An in-memory layer in front of a distributed store, with invalidations carried by a
/// [`RedisBackplane`].
/// One store holds values of every type: `get::<User>` and `set(&key, user, None)` name the type
/// per call.
///
/// Reads try the local layer, then the distributed one, and copy a distributed hit into the local
/// layer with its original creation time and TTL. Writes and removals evict the local copy, update
/// the distributed store, evict it again, and publish an invalidation so other nodes evict theirs.
/// Reading a local copy as another Rust type goes to the distributed store instead, so two types
/// that fit the same JSON can both read it.
///
/// The store applies other nodes' invalidations on a backplane worker it starts on its first call
/// inside a tokio runtime, or in `connect`, and stops when its last clone drops. Until that worker
/// has subscribed, and while the subscription is lost, the store reads through to the distributed
/// store.
pub struct HybridCacheStore<D: CacheStore + 'static, C: Clock = SystemClock> {
    distributed_store: Arc<D>,
    state: Arc<State<C>>,
    backplane: Arc<RedisBackplane>,
    /// The store's own TTL, refresh and lock settings, which both layers follow.
    cache_settings: CacheSettings,
    worker_settings: BackplaneWorkerSettings,
    /// The backplane worker. It holds `state` but not this, so it doesn't keep itself running.
    worker: Worker,
    reads: &'static Reads,
}

// Clones share the layers, the backplane and the worker, so they are one store.
impl<D: CacheStore + 'static, C: Clock> Clone for HybridCacheStore<D, C> {
    fn clone(&self) -> Self {
        Self {
            distributed_store: Arc::clone(&self.distributed_store),
            state: Arc::clone(&self.state),
            backplane: Arc::clone(&self.backplane),
            cache_settings: self.cache_settings.clone(),
            worker_settings: self.worker_settings.clone(),
            worker: self.worker.clone(),
            reads: self.reads,
        }
    }
}

// Shows the name, the node id and whether local reads are on; never a value.
impl<D: CacheStore + 'static, C: Clock> std::fmt::Debug for HybridCacheStore<D, C> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HybridCacheStore")
            .field("name", self.name())
            .field("node_id", self.node_id())
            .field("local_reads_enabled", &self.local_reads_enabled())
            .finish_non_exhaustive()
    }
}

impl<D: CacheStore + 'static> HybridCacheStore<D> {
    /// Builds the store over `distributed_store`, with its invalidations on `backplane`, and the
    /// local layer on each node from `settings.local`, named as the distributed store is.
    /// `settings` also gives the store's TTL, refresh and lock settings and the backplane
    /// worker's; `settings.distributed` is for [`connect`](Self::connect), which builds the Redis
    /// store, so set those on the store you pass here. Fails when a setting is out of range, or
    /// when `distributed_store` has TTL, refresh or lock settings of its own, which hybrid calls
    /// wouldn't read.
    ///
    /// On a Redis Cluster, the store takes a cluster connection and the backplane the cluster
    /// client, on RESP3 for its sharded pub/sub:
    ///
    /// ```no_run
    /// use sisa_caching_hybrid::{HybridCacheSettings, HybridCacheStore, RedisBackplane};
    /// use sisa_caching_redis::{
    ///     RedisCacheStore,
    ///     redis::{ProtocolVersion, cluster::ClusterClientBuilder},
    /// };
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let client = ClusterClientBuilder::new(vec!["redis://10.0.0.1:6379"])
    ///     .use_protocol(ProtocolVersion::RESP3)
    ///     .build()?;
    ///
    /// let cache = HybridCacheStore::new(
    ///     RedisCacheStore::new("app", client.get_async_connection().await?)?,
    ///     RedisBackplane::cluster(client).await?,
    ///     &HybridCacheSettings::default(),
    /// )?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Behind Sentinel, [`RedisBackplane::sentinel`] shows the setup. On one server,
    /// [`connect`](Self::connect) builds every part from a URL.
    pub fn new(
        distributed_store: D,
        backplane: RedisBackplane,
        settings: &HybridCacheSettings,
    ) -> Result<Self> {
        Self::new_on_clock(distributed_store, backplane, settings, SystemClock)
    }
}

impl<D: CacheStore + 'static, C: Clock> HybridCacheStore<D, C> {
    /// Builds the store as [`new`](HybridCacheStore::new) does, with its local layer on `clock`, so
    /// tests can move time.
    #[doc(hidden)]
    pub fn new_on_clock(
        distributed_store: D,
        backplane: RedisBackplane,
        settings: &HybridCacheSettings,
        clock: C,
    ) -> Result<Self> {
        settings.validate()?;

        if distributed_store.cache_settings() != &CacheSettings::default() {
            return Err(CacheError::invalid_settings(
                "distributed_store",
                "has TTL, refresh or lock settings of its own, which hybrid calls don't read; \
                 set them in HybridCacheSettings instead",
            )
            .into());
        }

        let name = distributed_store.name();
        let local_store = InMemoryCacheStore::new(name.as_str())?
            .with_clock(clock)
            .with_settings(settings.local_store_settings())?
            .with_local_ttl(settings.local.ttl());
        let backplane = backplane.for_store(name);
        let reads = METRICS.reads(name, Layer::Hybrid);

        Ok(Self {
            distributed_store: Arc::new(distributed_store),
            state: Arc::new(State::new(local_store)),
            backplane: Arc::new(backplane),
            cache_settings: settings.common.clone(),
            worker_settings: settings.worker.clone(),
            worker: Worker::new(),
            reads,
        })
    }

    /// Starts the backplane worker now instead of on the store's first call, as `connect` does,
    /// so local reads come on, and the `sisa.caching.backplane.connected` gauge reports, before
    /// the first call. It does nothing outside a tokio runtime or once the worker has started.
    pub fn start_worker(&self) {
        self.worker
            .start("hybrid-cache-backplane-worker", self.name(), || {
                let state = Arc::clone(&self.state);
                let backplane = Arc::clone(&self.backplane);

                BackplaneWorker::new(state, backplane, self.worker_settings.clone()).run()
            });
    }

    /// This store's id, shared by its clones. It goes out with every invalidation, so the store
    /// skips its own.
    pub fn node_id(&self) -> &NodeId {
        &self.state.node_id
    }

    /// The local layer, for inspection. Writing through it skips the distributed store and the
    /// backplane.
    #[doc(hidden)]
    pub fn local_store(&self) -> &InMemoryCacheStore<C> {
        &self.state.local_store
    }

    /// The distributed layer, for inspection. Writing through it skips the local layer and the
    /// backplane, so other nodes keep their copies.
    #[doc(hidden)]
    pub fn distributed_store(&self) -> &D {
        &self.distributed_store
    }

    /// Whether reads use the local layer: only while the backplane worker is subscribed.
    pub fn local_reads_enabled(&self) -> bool {
        self.state.local_reads_enabled()
    }

    /// Tells the other nodes that this one is stopping, so they forget it instead of clearing
    /// their local layers once its heartbeats have stopped for `heartbeat_timeout_ms`. Call it
    /// once, as the application shuts down, after its last write.
    ///
    /// It waits up to two seconds for the writes still running, even those whose caller gave up,
    /// to publish their invalidations. Then it publishes one `stopping` message that counts every
    /// write, so a node that missed one clears, and stops the backplane worker for good, for every
    /// clone, so reads go to the distributed store from then on. The stop runs in a task of its
    /// own, so a caller that gives up on it doesn't cut it short, and every call, at the same time
    /// or later, returns once it is done. A write after it still works, but the other nodes clear
    /// once more when they don't hear from this node again. A node that stops without calling it
    /// costs every other node one clear.
    pub async fn shutdown(&self) {
        {
            let mut stopped = self.state.stopped.lock().await;

            if !*stopped {
                // Taken while no heartbeat is publishing, and every later one sees it and ends, so
                // no heartbeat follows the stop message.
                *stopped = true;
                // In a task of its own, so a caller that gives up can't leave the store half
                // stopped.
                tokio::spawn(
                    stop(
                        Arc::clone(&self.state),
                        Arc::clone(&self.backplane),
                        self.worker.clone(),
                    )
                    .in_current_span(),
                );
            }
        }

        // Every call, and not only the one that started it, returns once the stop is done.
        self.state.shut_down.wait().await;
    }

    /// Runs one write or removal on the distributed store. The local copies are evicted before
    /// and after it, as other nodes' invalidations evict them, and the other nodes are told once
    /// it is done, even when it fails or panics, since a write without a reply may still have
    /// been applied. The call returns once the write is done and no local copy from before it
    /// can be read; the invalidation is published, and retried, in the background, so a
    /// backplane that can't be reached doesn't slow writes down. It runs in a `hybrid write`
    /// span, whose context the invalidation carries to the other nodes.
    #[tracing::instrument(name = "hybrid write", skip_all, fields(store = %self.name(), kind = invalidation.kind()), err(level = "info"))]
    async fn mutate<T, Fut>(
        &self,
        invalidation: Invalidation,
        operation: impl FnOnce(Arc<D>) -> Fut,
    ) -> Result<T>
    where
        T: Send + 'static,
        Fut: Future<Output = std::result::Result<T, D::Error>> + Send + 'static,
    {
        // A node that only writes still runs its worker.
        self.start_worker();

        let message = BackplaneMessage::new(self.node_id().clone(), invalidation);
        let store = self.name().clone();
        let state = Arc::clone(&self.state);
        let backplane = Arc::clone(&self.backplane);

        state.apply(&message.invalidation).await;

        let write = operation(Arc::clone(&self.distributed_store));

        // Numbered before the write runs, so a `shutdown` while it runs, even after its caller gave
        // up, waits for its publish and counts it.
        let publishing = state.sequence.start();

        // Spawned, so a caller that gives up half way can't stop other nodes from hearing of it.
        let task = tokio::spawn(
            async move {
                // In its own task too, so a write that panics is still evicted and published below.
                let result = tokio::spawn(write.in_current_span()).await;

                state.apply(&message.invalidation).await;
                tokio::spawn(publish(publishing, backplane, store, message).in_current_span());

                result
            }
            .in_current_span(),
        );

        task.await??.map_err(HybridCacheError::distributed)
    }
}

// The cache API: reads try the local layer first, and writes go to Redis, then to every node.
impl<D: CacheStore + 'static, C: Clock> CacheStore for HybridCacheStore<D, C> {
    type Error = HybridCacheError;
    // The local layer's clock, which the store's writes and refresh checks read.
    type Clock = C;

    fn name(&self) -> &KeyPrefix {
        self.distributed_store.name()
    }

    fn cache_settings(&self) -> &CacheSettings {
        &self.cache_settings
    }

    fn locker(&self) -> &Locker {
        &self.state.locker
    }

    fn clock(&self) -> &C {
        self.local_store().clock()
    }

    #[tracing::instrument(level = "debug", name = "hybrid get", skip_all, fields(key = %self.name().format_key(key)), err(level = "debug"))]
    async fn get_entry<CV: CacheValue>(&self, key: &str) -> Result<Option<CacheEntry<CV>>> {
        self.start_worker();

        let entry = match self.state.read::<CV>(key).await {
            Some(entry) => Some(entry),
            // On the heap, as it holds the distributed read: a local hit never builds or moves it.
            None => {
                Box::pin(async {
                    // Taken before the distributed read, so an invalidation that comes in during it
                    // keeps the value out of the local layer.
                    let generation = self.state.generation(key);
                    let entry = self
                        .distributed_store
                        .get_entry::<CV>(key)
                        .await
                        .map_err(HybridCacheError::distributed)?;

                    // No copy while local reads are off, as when the store is stopped or its
                    // backplane is down: `promote` would only drop it. It checks again under its
                    // turn.
                    if let Some(entry) = &entry
                        && self.state.local_reads_enabled()
                    {
                        self.state.promote(key, generation, entry.clone()).await;
                    }

                    Ok::<_, HybridCacheError>(entry)
                })
                .await?
            }
        };
        self.reads.record(entry.is_some());

        Ok(entry)
    }

    async fn set_entry<CV: CacheValue>(&self, key: &str, entry: CacheEntry<CV>) -> Result<()> {
        let key = key.to_owned();

        self.mutate(Invalidation::Key(key.clone()), async move |distributed| {
            distributed.set_entry(&key, entry).await
        })
        .await
    }

    async fn remove(&self, key: &str) -> Result<bool> {
        let key = key.to_owned();

        self.mutate(Invalidation::Key(key.clone()), async move |distributed| {
            distributed.remove(&key).await
        })
        .await
    }

    async fn remove_prefix(&self, prefix: &str) -> Result<usize> {
        let prefix = KeyPrefix::new(prefix)?;

        self.mutate(
            Invalidation::Prefix(prefix.clone()),
            async move |distributed| distributed.remove_prefix(prefix.as_str()).await,
        )
        .await
    }

    async fn clear(&self) -> Result<()> {
        self.mutate(Invalidation::Clear, async move |distributed| {
            distributed.clear().await
        })
        .await
    }
}

impl RedisHybridCacheStore {
    /// Connects every part to one Redis server, such as `redis://127.0.0.1:6379` (`rediss://`
    /// with the `tls-rustls` or `tls-native-tls` feature): the local layer, the Redis store and
    /// the backplane, each with its part of `settings`, and starts the backplane worker with
    /// `settings.worker`. `info` is a URL or a `redis::ConnectionInfo`. Its connections use
    /// `connection_config`: while Redis is down a call fails at once. For other timeouts, a client
    /// built by hand, a cluster or Sentinel, build the parts and use `new`.
    pub async fn connect(
        name: &str,
        info: impl IntoConnectionInfo,
        settings: &HybridCacheSettings,
    ) -> Result<Self> {
        settings.validate()?;

        let info = info
            .into_connection_info()
            .map_err(HybridCacheError::distributed)?;
        let distributed = RedisCacheStore::connect(name, info.clone())
            .await
            .map_err(HybridCacheError::distributed)?
            .with_settings(settings.distributed_store_settings())?;
        let backplane = RedisBackplane::connect(info)
            .await
            .map_err(HybridCacheError::distributed)?;
        let store = Self::new(distributed, backplane, settings)?;

        store.start_worker();
        Ok(store)
    }
}

// Marks this as one of the stores that may implement `CacheStore`.
impl<D: CacheStore + 'static, C: Clock> sisa_caching_core::sealed::Sealed
    for HybridCacheStore<D, C>
{
}
