//! A cache store on a Redis-family server: Redis, Valkey, Dragonfly, Garnet, or anything that
//! speaks the same commands.
//!
//! Each key is a hash with these fields, so other languages can read and write it too:
//!
//! | Field | Value |
//! | --- | --- |
//! | `value` | The value as UTF-8 JSON, such as `{"id":42,"name":"Ada"}` |
//! | `created_at` | When it was written, in Unix milliseconds |
//! | `ttl_ms` | Optional. Its TTL in milliseconds; leave it out when it never expires |
//!
//! The key is `{store}::{key}`, such as `app::user:42`. The store replaces the hash and sets its
//! `PEXPIRE` in one transaction, and other writers should too. Reads decode `value` into the type
//! you name, so compatible JSON written by another language reads fine. A hash without `value`
//! reads as a miss, and a field that isn't an integer fails the read.
//!
//! On one server, connect from a URL. `rediss://` URLs need the `tls-rustls` or
//! `tls-native-tls` feature and trust the system's root certificates; for a private CA or a
//! client certificate, use [`RedisCacheStore::from_client`].
//!
//! ```no_run
//! use sisa_caching_core::{CacheSettings, CacheStore};
//! use sisa_caching_redis::{RedisCacheSettings, RedisCacheStore};
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let cache = RedisCacheStore::connect("app", "redis://127.0.0.1:6379")
//!     .await?
//!     .with_settings(RedisCacheSettings {
//!         common: CacheSettings {
//!             ttl_secs: Some(300),
//!             ..Default::default()
//!         },
//!         ..Default::default()
//!     })?;
//!
//! let user = cache
//!     .get_or_set("user:v1:42", || async { Ok::<_, std::io::Error>("Ada".to_string()) }, None)
//!     .await?;
//! # Ok(())
//! # }
//! ```
//!
//! For other servers, build a [`RedisConnection`] with the re-exported [`redis`] crate and pass it
//! to `RedisCacheStore::new`:
//!
//! | Server | Connection |
//! | --- | --- |
//! | One server, with or without replicas | `redis::aio::ConnectionManager` (what `connect` builds, with [`connection_config`]), or a `MultiplexedConnection`, which doesn't reconnect |
//! | Redis Cluster | `redis::cluster_async::ClusterConnection`; `remove_prefix` and `clear` scan every primary |
//! | Sentinel | [`SentinelConnection`] |
//!
//! Every read and write goes to a primary, so a read never returns a value older than the last
//! write, except behind Sentinel just after a failover, as [`SentinelConnection`] says.
//! Replication is still asynchronous: a write the primary acknowledged just before a failover can
//! be lost, and the previous value is served until it expires.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]
#![warn(missing_docs)]

mod codec;
pub mod connection;
pub mod error;
pub mod settings;
pub mod store;

/// The redis-rs crate this one is built on, for building connections without depending on it.
pub use redis;

pub use connection::{RedisConnection, SentinelConnection, connection_config};
pub use error::RedisCacheError;
pub use settings::RedisCacheSettings;
pub use store::RedisCacheStore;
