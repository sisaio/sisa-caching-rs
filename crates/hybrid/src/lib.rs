//! A two-level cache: an in-memory layer on each node in front of a shared Redis-family store.
//!
//! Reads try the local layer first, then the shared store. Writes update the shared store, drop the
//! local copy, and publish an invalidation on a backplane (Redis pub/sub), so every other node
//! drops its copy too. Each hybrid store listens for those invalidations on a worker it starts
//! itself, on its first call or in `connect`, and stops when its last clone drops. Until the
//! worker is listening, the store skips the local layer and reads the shared store.
//!
//! On one server, [`HybridCacheStore::connect`] builds every part from a URL and one
//! [`HybridCacheSettings`], which can come straight from a config file. `rediss://` URLs need the
//! `tls-rustls` or `tls-native-tls` feature. For a private CA or a client certificate, build the
//! parts with `new`, and the backplane with [`RedisBackplane::from_client`].
//!
//! ```no_run
//! use sisa_caching_core::CacheStore;
//! use sisa_caching_hybrid::{HybridCacheSettings, HybridCacheStore};
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // Local copies live at most 60 seconds, whatever their TTL, unless `local.max_ttl_secs` says
//! // otherwise.
//! let mut settings = HybridCacheSettings::default();
//! settings.distributed.common.ttl_secs = Some(300);
//!
//! let cache = HybridCacheStore::connect("app", "redis://127.0.0.1:6379", &settings).await?;
//!
//! cache.set("user:v1:42", "Ada".to_string(), None).await?;
//! # Ok(())
//! # }
//! ```
//!
//! For a Redis Cluster, Sentinel or another distributed store, build the three parts and compose
//! them with [`HybridCacheStore::new`], then give it `settings.worker` with
//! [`with_worker_settings`](HybridCacheStore::with_worker_settings). Build the local store with
//! `HybridCacheSettings::default().local`, or its own `max_ttl_secs`, to keep the 60-second cap
//! on stale copies; `InMemoryCacheStore::new` alone has none. On a cluster, give the store a
//! `ClusterConnection` and the backplane the cluster client ([`RedisBackplane::cluster`]), which
//! uses sharded pub/sub.
//!
//! Behind Sentinel, share one `SentinelConnection` between the store
//! ([`RedisCacheStore`](sisa_caching_redis::RedisCacheStore)) and the backplane
//! ([`RedisBackplane::sentinel`]), so a write that finds the old primary demoted moves the store
//! and the backplane's publishing to the new one.
//!
//! A write returns once the shared store has answered; its invalidation is published in the
//! background. If that fails after three tries, a warning is logged. Other nodes then keep their
//! old copy until the local `max_ttl_secs` (60 seconds by default) drops it.
//!
//! Each invalidation carries the writer's trace context, so a write and the `hybrid
//! apply_invalidation` spans it causes on other nodes land in one trace. This needs a text map
//! propagator installed at startup, such as OpenTelemetry's `TraceContextPropagator`; without one
//! the message carries no context and each node starts its own trace.
//!
//! # How stale a local copy can get
//!
//! Reads copy a shared hit into the local layer with its original creation time and TTL. Writes
//! and removals evict the local copy, update the shared store, evict again, and publish. Delivery
//! is at most once, so when the subscription fails (a dropped connection, a failed ping, a message
//! that can't be read) the worker clears the local layer and reads go to the shared store until it
//! has subscribed again. [`local_reads_enabled`](store::HybridCacheStore::local_reads_enabled)
//! reports which mode the store is in.
//!
//! Each invalidation bumps a generation for its key's slot before it evicts. A read copies into
//! the local layer only if the generation hasn't moved since it started, and an invalidation
//! waits for a copy in progress, so a slow read can't bring back a value that was just
//! invalidated.
//!
//! The local `max_ttl_secs` is the only bound when an invalidation never arrives: every publish
//! attempt failed, the writer crashed before publishing, or a Redis write timed out on the client
//! but was applied later. `HybridCacheSettings` sets it to 60 seconds unless told otherwise; a
//! local store built without those settings has none. `0` removes the bound. It doesn't change
//! freshness: an entry with a 100-second TTL still refreshes at 80 seconds with a
//! `refresh_threshold` of `0.8`.
//!
//! # Invalidations from other languages
//!
//! The Redis backplane publishes JSON on its channel (`sisa-caching:backplane` by default):
//!
//! ```json
//! {"v":1,"source":"node-7","store":"app","kind":"key","target":"user:42"}
//! ```
//!
//! `kind` is `key`, `prefix` or `clear`. `target` is the key or prefix without the store name;
//! `clear` has none. `source` is any id unique to the writing process. An optional `trace` object
//! carries propagation fields such as `traceparent`. A writer in another language publishes this
//! after each write, so hybrid nodes drop their copies.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod backplane;
pub mod error;
pub mod settings;
pub mod store;
mod worker;

pub use backplane::RedisBackplane;
pub use error::HybridCacheError;
pub use settings::HybridCacheSettings;
pub use store::{HybridCacheStore, RedisHybridCacheStore};
