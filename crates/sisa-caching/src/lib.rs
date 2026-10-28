//! Caching for Rust services: an in-memory store, a store on Redis, Valkey, Dragonfly or Garnet,
//! and a hybrid that keeps a local copy on each node and drops it when another node writes. Every
//! store has the same calls (`get`, `set`, `get_or_set`, `remove`, `remove_prefix` and `clear`)
//! through the [`CacheStore`] trait, so switching stores changes one line.
//!
//! No store is on by default; turn on the ones you use:
//!
//! | Feature | Adds |
//! | --- | --- |
//! | `memory` | `InMemoryCacheStore` |
//! | `redis` | `RedisCacheStore`, and the `redis` crate it is built on |
//! | `hybrid` | `HybridCacheStore` and its `BackplaneWorker`; turns on `memory` and `redis` |
//! | `tls-rustls` | `rediss://` URLs through rustls; turns on `redis` |
//! | `tls-native-tls` | `rediss://` URLs through the system's TLS library; turns on `redis` |
//!
//! ```
//! # #[cfg(feature = "memory")]
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use sisa_caching::{CacheStore, InMemoryCacheStore};
//!
//! // From its first write on, it drops expired entries nobody reads again every 60 seconds.
//! let cache = InMemoryCacheStore::new("app")?;
//!
//! let name = cache
//!     .get_or_set("user:v1:42", || async { Ok::<_, std::io::Error>("Ada".to_string()) }, None)
//!     .await?;
//!
//! assert_eq!(name, "Ada");
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "memory"))]
//! # fn main() {}
//! ```
//!
//! A hybrid on one Redis server, with the worker it needs:
//!
//! ```no_run
//! # #[cfg(feature = "hybrid")]
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! use sisa_caching::{BackplaneWorker, CacheStore, HybridCacheSettings, HybridCacheStore, WorkerHost};
//!
//! let settings = HybridCacheSettings::default();
//! let cache = HybridCacheStore::connect("app", "redis://127.0.0.1:6379", &settings).await?;
//!
//! // Keep the host alive as long as the cache; dropping it stops the worker.
//! let workers = WorkerHost::new();
//! workers.register(BackplaneWorker::new(&cache).with_settings(settings.worker.clone()));
//!
//! cache.set("user:v1:42", "Ada".to_string(), None).await?;
//! # Ok(())
//! # }
//! ```
//!
//! Each part is also its own crate (`sisa-caching-core`, `-memory`, `-redis`, `-hybrid`), whose
//! docs cover the details: key and entry formats, Cluster and Sentinel, the backplane, and the
//! metrics. Depend on one of those directly to implement a store or a backplane of your own.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub use sisa_caching_core::{
    CacheEntry, CacheError, CacheKey, CacheOptions, CacheSettings, CacheStore, CacheValue,
    KeyPrefix, WorkerHost, WorkerService,
    cache::PurgeableStore,
    clock::{Clock, SystemClock},
    worker::CancellationToken,
};

#[cfg(feature = "memory")]
pub use sisa_caching_memory::{InMemoryCacheSettings, InMemoryCacheStore};

#[cfg(feature = "redis")]
pub use sisa_caching_redis::{
    RedisCacheError, RedisCacheSettings, RedisCacheStore, RedisConnection, SentinelConnection,
    connection_config, redis,
};

#[cfg(feature = "hybrid")]
pub use sisa_caching_hybrid::{
    BackplaneWorker, HybridCacheError, HybridCacheSettings, HybridCacheStore, RedisBackplane,
    RedisHybridCacheStore,
    settings::{BackplaneWorkerSettings, RedisBackplaneSettings},
};
