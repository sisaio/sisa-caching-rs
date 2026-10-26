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
//! `tls-native-tls` feature.
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
//! `rediss://` trusts the system's root certificates. For a private CA or a client certificate,
//! build the client with them:
//!
//! ```no_run
//! # #[cfg(feature = "tls-rustls")]
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! use sisa_caching_redis::{
//!     RedisCacheStore,
//!     redis::{Client, ClientTlsConfig, TlsCertificates},
//! };
//!
//! let client = Client::build_with_tls(
//!     "rediss://cache.internal:6380",
//!     TlsCertificates {
//!         client_tls: Some(ClientTlsConfig {
//!             client_cert: std::fs::read("client.crt")?,
//!             client_key: std::fs::read("client.key")?,
//!         }),
//!         root_cert: Some(std::fs::read("ca.crt")?),
//!     },
//! )?;
//! let cache = RedisCacheStore::from_client("app", client).await?;
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
//! write. The exception is Sentinel: after a failover, a connection that hasn't written since keeps
//! reading from the old primary, now a replica, until its next write moves it. Replication is
//! still asynchronous: a write the primary acknowledged just before a failover can be lost, and the
//! previous value is served until it expires.
//!
//! Behind Sentinel:
//!
//! ```no_run
//! use sisa_caching_redis::{
//!     RedisCacheStore, SentinelConnection,
//!     redis::sentinel::{SentinelClient, SentinelServerType},
//! };
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let sentinel = SentinelClient::build(
//!     vec!["redis://10.0.0.1:26379", "redis://10.0.0.2:26379"],
//!     String::from("mymaster"),
//!     None,
//!     SentinelServerType::Master,
//! )?;
//! let conn = SentinelConnection::connect(sentinel).await?;
//! let cache = RedisCacheStore::new("app", conn)?;
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

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
