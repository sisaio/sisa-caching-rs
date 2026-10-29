//! The hybrid store's errors.

use sisa_caching_core::cache::{CacheError, KeyPrefix};

type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Errors from the hybrid store, by the layer they came from.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HybridCacheError {
    /// A bad store name or setting, or a `CacheOptions` TTL over 365 days, rejected before anything
    /// is evicted or written. Local type mismatches read through, and the distributed store's
    /// errors are `Distributed`.
    #[error(transparent)]
    Cache(#[from] CacheError),

    /// `HybridCacheStore::new` was given a local and a distributed store with different names.
    #[error("local store `{local}` and distributed store `{distributed}` must have the same name")]
    #[non_exhaustive]
    StoreNameMismatch {
        local: KeyPrefix,
        distributed: KeyPrefix,
    },

    /// An error from the distributed store, boxed. For a
    /// [`RedisHybridCacheStore`](crate::RedisHybridCacheStore), downcast it to reach the Redis
    /// error:
    ///
    /// ```
    /// use sisa_caching_hybrid::HybridCacheError;
    /// use sisa_caching_redis::RedisCacheError;
    ///
    /// fn redis_error(error: &HybridCacheError) -> Option<&RedisCacheError> {
    ///     match error {
    ///         HybridCacheError::Distributed(source) => source.downcast_ref(),
    ///         _ => None,
    ///     }
    /// }
    /// ```
    #[error("distributed layer: {0}")]
    Distributed(#[source] BoxError),

    /// The write task panicked, or the runtime shut down before it finished.
    #[error("write stopped before reporting its result")]
    WriteStopped(#[from] tokio::task::JoinError),
}

impl HybridCacheError {
    pub(crate) fn distributed(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Distributed(Box::new(error))
    }
}

pub type Result<T> = std::result::Result<T, HybridCacheError>;
