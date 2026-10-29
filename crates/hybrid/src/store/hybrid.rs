use std::{sync::Arc, time::Duration};

use redis::IntoConnectionInfo;
use sisa_caching_core::{
    cache::{CacheEntry, CacheStore, CacheValue, KeyPrefix},
    clock::Clock,
    locker::Locker,
    metrics::{Layer, METRICS},
    settings::CacheSettings,
    worker::Worker,
};
use sisa_caching_memory::store::InMemoryCacheStore;
use sisa_caching_redis::store::RedisCacheStore;
use tracing::Instrument;

use crate::{
    backplane::{BackplaneMessage, Invalidation, NodeId, RedisBackplane},
    error::{HybridCacheError, Result},
    settings::{BackplaneWorkerSettings, HybridCacheSettings, check_local_common},
    worker::BackplaneWorker,
};

use super::{RedisHybridCacheStore, state::State};

/// How many times a write tries to publish its invalidation, waiting 50 ms, then 100 ms, between
/// tries. A late or repeated invalidation only costs a cache miss, so retrying is always safe.
const PUBLISH_ATTEMPTS: u32 = 3;

/// An in-memory layer in front of a distributed store, with invalidations carried by a
/// [`RedisBackplane`].
/// One store holds values of every type: `get::<User>` and `set(&key, user, None)` name the type
/// per call.
///
/// Reads try the local layer, then the distributed one, and copy a distributed hit into the local
/// layer with its original creation time and TTL. Writes and removals evict the local copy, update
/// the distributed store, evict it again, and publish an invalidation so other nodes evict theirs.
/// A local Rust type mismatch reads through to the distributed store, whose decoding rules decide
/// compatibility, so different Rust types can read the same shared JSON when their schemas fit.
///
/// The store applies other nodes' invalidations on a backplane worker it starts on its first call
/// inside a tokio runtime, or in `connect`, and stops when its last clone drops. Until that worker
/// has subscribed, and while the subscription is lost, the store reads through to the distributed
/// store.
pub struct HybridCacheStore<D: CacheStore + 'static> {
    distributed_store: Arc<D>,
    state: Arc<State>,
    backplane: Arc<RedisBackplane>,
    worker_settings: BackplaneWorkerSettings,
    /// The backplane worker. It holds `state` but not this, so it doesn't keep itself running.
    worker: Worker,
}

impl<D: CacheStore + 'static> Clone for HybridCacheStore<D> {
    fn clone(&self) -> Self {
        Self {
            distributed_store: Arc::clone(&self.distributed_store),
            state: Arc::clone(&self.state),
            backplane: Arc::clone(&self.backplane),
            worker_settings: self.worker_settings.clone(),
            worker: self.worker.clone(),
        }
    }
}

/// The name, the node id and whether local reads are on; never a value.
impl<D: CacheStore + 'static> std::fmt::Debug for HybridCacheStore<D> {
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
    /// Composes the configured layers without changing their settings. Names must match, and the
    /// local store's `common` settings must stay unset, since hybrid calls use the distributed
    /// store's.
    ///
    /// The local store's `max_ttl_secs` is the longest a node serves a copy whose invalidation
    /// never arrived. `InMemoryCacheStore::new` leaves it off, so build the local store with the
    /// hybrid default of 60 seconds:
    ///
    /// ```
    /// # use sisa_caching_hybrid::HybridCacheSettings;
    /// # use sisa_caching_memory::InMemoryCacheStore;
    /// let local =
    ///     InMemoryCacheStore::new("app")?.with_settings(HybridCacheSettings::default().local)?;
    /// # Ok::<(), sisa_caching_core::CacheError>(())
    /// ```
    ///
    /// The store starts its backplane worker on its first call, with the default
    /// [`BackplaneWorkerSettings`] unless [`with_worker_settings`](Self::with_worker_settings)
    /// gives others, and the local store drops its expired copies on its own. The
    /// [crate docs](crate) show the whole setup.
    pub fn new(
        local_store: InMemoryCacheStore,
        distributed_store: D,
        backplane: RedisBackplane,
    ) -> Result<Self> {
        check_local_common(local_store.settings())?;

        if local_store.name() != distributed_store.name() {
            return Err(HybridCacheError::StoreNameMismatch {
                local: local_store.name().clone(),
                distributed: distributed_store.name().clone(),
            });
        }

        Ok(Self {
            distributed_store: Arc::new(distributed_store),
            state: Arc::new(State::new(local_store)),
            backplane: Arc::new(backplane),
            worker_settings: BackplaneWorkerSettings::default(),
            worker: Worker::new(),
        })
    }

    /// How the backplane worker waits before subscribing again; `connect` takes it from
    /// `settings.worker`. Set it before the store's first call: the worker reads it once, when it
    /// starts.
    pub fn with_worker_settings(mut self, settings: BackplaneWorkerSettings) -> Self {
        self.worker_settings = settings;
        self
    }

    /// Starts the backplane worker now instead of on the store's first call, as `connect` does,
    /// so local reads come on, and the `sisa.caching.backplane.connected` gauge reports, before
    /// the first call. It does nothing outside a tokio runtime or once the worker has started.
    pub fn start_worker(&self) {
        self.worker.start("hybrid-cache-backplane-worker", || {
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
    pub fn local_store(&self) -> &InMemoryCacheStore {
        &self.state.local_store
    }

    /// The distributed layer, for inspection. Writing through it skips the local layer and the
    /// backplane, so other nodes keep their copies.
    pub fn distributed_store(&self) -> &D {
        &self.distributed_store
    }

    /// Whether reads use the local layer: only while the backplane worker is subscribed.
    pub fn local_reads_enabled(&self) -> bool {
        self.state.local_reads_enabled()
    }

    /// Runs one write or removal on the distributed store. The local copies are evicted before
    /// and after it, and the other nodes are told once it is done, even when it fails or panics,
    /// since a write without a reply may still have been applied. The call returns once the write
    /// and the second eviction are done; the invalidation is published, and retried, in the
    /// background, so a backplane that can't be reached doesn't slow writes down. It runs in a
    /// `hybrid write` span, whose context the invalidation carries to the other nodes.
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

        let message =
            BackplaneMessage::new(self.node_id().clone(), self.name().clone(), invalidation);
        let state = Arc::clone(&self.state);
        let backplane = Arc::clone(&self.backplane);

        state.invalidate(&message.invalidation).await;

        let write = operation(Arc::clone(&self.distributed_store));

        // Spawned, so a caller that gives up half way can't stop other nodes from hearing of it.
        let task = tokio::spawn(
            async move {
                // In its own task too, so a write that panics is still evicted and published below.
                let result = tokio::spawn(write.in_current_span()).await;

                state.invalidate(&message.invalidation).await;
                tokio::spawn(publish(state, backplane, message).in_current_span());

                result
            }
            .in_current_span(),
        );

        task.await??.map_err(HybridCacheError::distributed)
    }
}

/// Publishes one invalidation under the store's next sequence number, retrying with a growing wait.
/// When every try fails, the other nodes see the number missing at this node's next heartbeat and
/// clear their local layers.
async fn publish(state: Arc<State>, backplane: Arc<RedisBackplane>, mut message: BackplaneMessage) {
    let publishing = state.sequence.start();
    let mut delay = Duration::from_millis(50);

    message.seq = Some(publishing.seq);

    for attempt in 1..=PUBLISH_ATTEMPTS {
        match backplane.publish(&message).await {
            Ok(()) => return,
            Err(error) if attempt == PUBLISH_ATTEMPTS => {
                METRICS.publish_failed(&message.store);
                tracing::warn!(
                    store = %message.store,
                    %error,
                    kind = message.invalidation.kind(),
                    "could not publish an invalidation; other nodes clear their local layers at the next heartbeat"
                );
            }
            Err(error) => {
                tracing::debug!(store = %message.store, %error, attempt, "publish failed; retrying");

                tokio::time::sleep(delay).await;
                delay *= 2;
            }
        }
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
        KeyPrefix::validate(name)?;

        let info = info
            .into_connection_info()
            .map_err(HybridCacheError::distributed)?;
        let local = InMemoryCacheStore::new(name)?.with_settings(settings.local.clone())?;
        let distributed = RedisCacheStore::connect(name, info.clone())
            .await
            .map_err(HybridCacheError::distributed)?
            .with_settings(settings.distributed.clone())?;
        let backplane = RedisBackplane::connect(info, settings.backplane.clone())
            .await
            .map_err(HybridCacheError::distributed)?;
        let store =
            Self::new(local, distributed, backplane)?.with_worker_settings(settings.worker.clone());

        store.start_worker();
        Ok(store)
    }
}

impl<D: CacheStore + 'static> CacheStore for HybridCacheStore<D> {
    type Error = HybridCacheError;

    fn name(&self) -> &KeyPrefix {
        self.distributed_store.name()
    }

    fn cache_settings(&self) -> &CacheSettings {
        self.distributed_store.cache_settings()
    }

    fn locker(&self) -> &Locker {
        &self.state.locker
    }

    fn clock(&self) -> &dyn Clock {
        self.local_store().clock()
    }

    #[tracing::instrument(level = "debug", name = "hybrid get", skip_all, fields(key = %self.name().format_key(key)), err(level = "debug"))]
    async fn get_entry<CV: CacheValue>(&self, key: &str) -> Result<Option<CacheEntry<CV>>> {
        self.start_worker();

        let (generation, local) = self.state.read::<CV>(key).await;

        let entry = match local {
            Some(entry) => Some(entry),
            None => {
                let entry = self
                    .distributed_store
                    .get_entry::<CV>(key)
                    .await
                    .map_err(HybridCacheError::distributed)?;

                if let Some(entry) = &entry {
                    self.state.promote(key, generation, entry.clone()).await;
                }

                entry
            }
        };
        METRICS.read(self.name(), Layer::Hybrid, entry.is_some());

        Ok(entry)
    }

    async fn set_entry<CV: CacheValue>(&self, key: &str, entry: CacheEntry<CV>) -> Result<()> {
        let key = key.to_owned();

        self.mutate(
            Invalidation::Key(key.clone()),
            move |distributed| async move { distributed.set_entry(&key, entry).await },
        )
        .await
    }

    async fn remove(&self, key: &str) -> Result<bool> {
        let key = key.to_owned();

        self.mutate(
            Invalidation::Key(key.clone()),
            move |distributed| async move { distributed.remove(&key).await },
        )
        .await
    }

    async fn remove_prefix(&self, prefix: &str) -> Result<usize> {
        let prefix = KeyPrefix::new(prefix)?;

        self.mutate(
            Invalidation::Prefix(prefix.clone()),
            move |distributed| async move { distributed.remove_prefix(prefix.as_str()).await },
        )
        .await
    }

    async fn clear(&self) -> Result<()> {
        self.mutate(Invalidation::Clear, |distributed| async move {
            distributed.clear().await
        })
        .await
    }
}
