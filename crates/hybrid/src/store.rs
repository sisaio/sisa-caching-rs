//! The hybrid store.

mod hybrid;
pub(crate) mod state;

pub use hybrid::HybridCacheStore;

/// A hybrid store on Redis.
pub type RedisHybridCacheStore = HybridCacheStore<sisa_caching_redis::store::RedisCacheStore>;
