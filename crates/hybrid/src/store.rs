//! A hybrid composes a configured local store, distributed store, and backplane.

mod hybrid;
pub(crate) mod state;

pub use hybrid::HybridCacheStore;

/// A hybrid store on Redis, with a Redis backplane.
pub type RedisHybridCacheStore =
    HybridCacheStore<sisa_caching_redis::store::RedisCacheStore, crate::backplane::RedisBackplane>;
