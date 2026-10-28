use std::{sync::Arc, time::Duration};

use redis::IntoConnectionInfo;
use sisa_caching_core::{
    cache::{CacheEntry, CacheStore, CacheValue, KeyPrefix},
    clock::Clock,
    locker::Locker,
    metrics::{Layer, METRICS},
    settings::CacheSettings,
};
use sisa_caching_memory::store::InMemoryCacheStore;
use sisa_caching_redis::store::RedisCacheStore;
use tracing::Instrument;

use crate::{
    backplane::{Backplane, BackplaneMessage, Invalidation, NodeId, RedisBackplane},
    error::{HybridCacheError, Result},
    settings::{HybridCacheSettings, check_local_common},
};

use super::{RedisHybridCacheStore, state::State};

/// How many times a write tries to publish its invalidation, waiting 50 ms, then 100 ms, between
/// tries. A late or repeated invalidation only costs a cache miss, so retrying is always safe.
const PUBLISH_ATTEMPTS: u32 = 3;

/// An in-memory layer in front of a distributed store, with invalidations carried by a backplane.
/// One store holds values of every type: `get::<User>` and `set(&key, user, None)` name the type
/// per call.
///
/// Reads try the local layer, then the distributed one, and copy a distributed hit into the local
/// layer with its original creation time and TTL. Writes and removals evict the local copy, update
/// the distributed store, evict it again, and publish an invalidation so other nodes evict theirs.
/// A local Rust type mismatch reads through to the distributed store, whose decoding rules decide
/// compatibility, so different Rust types can read the same shared JSON when their schemas fit.
///
/// Until its backplane worker has subscribed, while the subscription is lost, and after the worker
/// stops, the store reads through to the distributed store.
pub struct HybridCacheStore<D: CacheStore + 'static, B: Backplane> {
    distributed_store: Arc<D>,
    state: Arc<State<B>>,
}

impl<D: CacheStore + 'static, B: Backplane> Clone for HybridCacheStore<D, B> {
    fn clone(&self) -> Self {
        Self {
            distributed_store: Arc::clone(&self.distributed_store),
            state: Arc::clone(&self.state),
        }
    }
}

/// The name, the node id and whether local reads are on; never a value.
impl<D: CacheStore + 'static, B: Backplane> std::fmt::Debug for HybridCacheStore<D, B> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HybridCacheStore")
            .field("name", self.name())
            .field("node_id", self.node_id())
            .field("local_reads_enabled", &self.local_reads_enabled())
            .finish_non_exhaustive()
    }
}

impl<D: CacheStore + 'static, B: Backplane> HybridCacheStore<D, B> {
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
    /// The store reads through to the distributed store until a
    /// [`BackplaneWorker`](crate::BackplaneWorker) has subscribed for it, so register one on a
    /// `WorkerHost`. The local store drops expired copies on its own. The [crate docs](crate)
    /// show the whole setup.
    pub fn new(
        local_store: InMemoryCacheStore,
        distributed_store: D,
        backplane: B,
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
            state: Arc::new(State::new(local_store, backplane)),
        })
    }

    pub(crate) fn state(&self) -> &Arc<State<B>> {
        &self.state
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

    /// Whether reads use the local layer: only while a backplane worker is subscribed.
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
        let message =
            BackplaneMessage::new(self.node_id().clone(), self.name().clone(), invalidation);
        let state = Arc::clone(&self.state);

        state.invalidate(&message.invalidation).await;

        let write = operation(Arc::clone(&self.distributed_store));

        // Spawned, so a caller that gives up half way can't stop other nodes from hearing of it.
        let task = tokio::spawn(
            async move {
                // In its own task too, so a write that panics is still evicted and published below.
                let result = tokio::spawn(write.in_current_span()).await;

                state.invalidate(&message.invalidation).await;
                tokio::spawn(publish(state, message).in_current_span());

                result
            }
            .in_current_span(),
        );

        task.await??.map_err(HybridCacheError::distributed)
    }
}

/// Publishes one invalidation, retrying with a growing wait. When every try fails, the other nodes
/// keep their copies until the local `max_ttl_secs` drops them.
async fn publish<B: Backplane>(state: Arc<State<B>>, message: BackplaneMessage) {
    let mut delay = Duration::from_millis(50);

    for attempt in 1..=PUBLISH_ATTEMPTS {
        match state.backplane.publish(&message).await {
            Ok(()) => return,
            Err(error) if attempt == PUBLISH_ATTEMPTS => {
                METRICS.publish_failed(&message.store);
                tracing::warn!(
                    store = %message.store,
                    %error,
                    kind = message.invalidation.kind(),
                    "could not publish an invalidation; other nodes may serve stale values"
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
    /// the backplane, each with its part of `settings`. `info` is a URL or a `redis::ConnectionInfo`. Register a
    /// `BackplaneWorker` with `settings.worker` next. Its connections use `connection_config`: while Redis is down a call fails at once.
    /// For other timeouts, a client built by hand, a cluster or Sentinel, build the parts and use
    /// `new`.
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

        Self::new(local, distributed, backplane)
    }
}

impl<D: CacheStore + 'static, B: Backplane> CacheStore for HybridCacheStore<D, B> {
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
