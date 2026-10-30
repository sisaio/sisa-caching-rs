//! A two-level cache: an in-memory layer on each node in front of a shared Redis-family store.
//!
//! Reads try the local layer first, then the shared store. Writes update the shared store, drop the
//! local copy, and publish an invalidation on a backplane (Redis pub/sub), so every other node
//! drops its copy too. Each store listens for those invalidations itself, and until it is
//! listening, reads skip the local layer.
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
//! them with [`HybridCacheStore::new`], which says how to keep the 60-second cap on local copies,
//! then give it `settings.worker` with
//! [`with_worker_settings`](HybridCacheStore::with_worker_settings). On a cluster, give the store
//! a `ClusterConnection` and the backplane the cluster client ([`RedisBackplane::cluster`]),
//! which uses sharded pub/sub.
//!
//! Each store publishes and listens on its own pub/sub channel, `<store>:backplane`. One store
//! holds values of every type, so one per application is usually enough. Run a second one when
//! some data needs other settings, such as another TTL or `max_entries`: with its own name, it
//! has its own local layer, worker and channel, and doesn't affect the first. Store names must
//! differ, also between deployments that share a Redis server, as pub/sub ignores the database.
//! Create each store once at startup and share it through clones, as each one opens a
//! subscription of its own.
//!
//! Behind Sentinel, share one `SentinelConnection` between the store
//! ([`RedisCacheStore`](sisa_caching_redis::RedisCacheStore)) and the backplane
//! ([`RedisBackplane::sentinel`]), so a write that finds the old primary demoted moves the store
//! and the backplane's publishing to the new one.
//!
//! Install a text map propagator at startup, such as OpenTelemetry's `TraceContextPropagator`, so
//! a write and the `hybrid apply_invalidation` spans it causes on other nodes share one trace.
//!
//! # How stale a local copy can get
//!
//! A write returns once the shared store has answered and this node can't read its old copy; the
//! other nodes drop theirs when its invalidation arrives, a moment later. When one is lost, as
//! when all three tries to publish it fail, they clear their local layers at the writer's next
//! heartbeat (`heartbeat_interval_ms`, 1 second by default), or once its heartbeats have stopped
//! for `heartbeat_timeout_ms` (5 seconds). While its subscription is down, a store reads only the
//! shared store; [`local_reads_enabled`](HybridCacheStore::local_reads_enabled) says which way it
//! reads.
//!
//! Call [`shutdown`](HybridCacheStore::shutdown) once as the application stops, after its last
//! write, so the other nodes forget this one instead of clearing when its heartbeats stop:
//!
//! ```no_run
//! # use sisa_caching_hybrid::{HybridCacheSettings, HybridCacheStore};
//! # async fn serve<T>(_: T) {}
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let cache = HybridCacheStore::connect("app", "redis://127.0.0.1:6379", &HybridCacheSettings::default()).await?;
//!
//! serve(cache.clone()).await;
//! cache.shutdown().await;
//! # Ok(())
//! # }
//! ```
//!
//! The local `max_ttl_secs` bounds what nothing shows, such as a Redis write that timed out on the
//! client but was applied later, or a writer in another language that sends no sequence numbers.
//! `HybridCacheSettings` sets it to 60 seconds; a local store built without those settings has
//! none, and `None` removes it. It doesn't change freshness: an entry with a 100-second TTL still
//! refreshes at 80 seconds with a `refresh_threshold` of `0.8`.
//!
//! # Invalidations from other languages
//!
//! The Redis backplane publishes JSON on the store's channel, `<store>:backplane`, such as
//! `app:backplane` for a store named `app`:
//!
//! ```json
//! {"v":1,"source":"node-7","store":"app","kind":"key","target":"user:42"}
//! ```
//!
//! `kind` is `key`, `prefix` or `clear`. `target` is the key or prefix without the store name;
//! `clear` has none. `source` is any id unique to the writing process. An optional `trace` object
//! carries propagation fields such as `traceparent`. A writer in another language publishes this
//! after each write, so hybrid nodes drop their copies.
//!
//! Hybrid nodes also number their invalidations in `seq` and publish `heartbeat` and `stopping`
//! messages, so the others notice one that goes missing. A writer in another language can leave
//! them all out: nodes apply its invalidations without tracking them.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]
#![warn(missing_docs)]

pub mod backplane;
pub mod error;
pub mod settings;
pub mod store;
mod worker;

pub use backplane::RedisBackplane;
pub use error::HybridCacheError;
pub use settings::HybridCacheSettings;
pub use store::{HybridCacheStore, RedisHybridCacheStore};
