//! The shared parts of Sisa Caching: keys, entries, settings, the [`CacheStore`] trait every store
//! implements, and the [`WorkerHost`] that runs background jobs.
//!
//! Applications depend on `sisa-caching`, which re-exports this crate's types; depend on this one
//! directly to implement a store. Every store has the same calls:
//! `get`, `set`, `get_or_set`, `remove`, `remove_prefix` and `clear`, plus `get_entry` and
//! `set_entry` for a value with its creation time and TTL.
//!
//! A key is any string, with levels separated by `:`, such as `user:42`. A store adds its name in
//! front with `::`, so the same key in two stores never collides. [`CacheKey`] builds a checked
//! key from a prefix and a key:
//!
//! ```
//! use sisa_caching_core::{CacheKey, KeyPrefix};
//!
//! let store = KeyPrefix::new("app")?;
//! let key = CacheKey::new("user:v1", 42)?;
//!
//! assert_eq!(&*key, "user:v1:42");
//! assert_eq!(store.format_key("user:v1:42"), "app::user:v1:42");
//! assert_eq!(store.format_key(&key), "app::user:v1:42");
//! # Ok::<(), sisa_caching_core::CacheError>(())
//! ```
//!
//! Stores trace through [`tracing`](https://docs.rs/tracing). Every store call gets a debug span
//! (`memory get`, `redis set`, `cache get_or_set`, ...) carrying the full key. The hybrid store
//! adds info spans that carry only the operation, never a key: `hybrid write`, `hybrid
//! apply_invalidation` on the nodes that receive it, and `hybrid resync`. A failed call marks its
//! span as an error. Stores log only the errors they swallow, as warnings that name the store but
//! never a key, such as a value that can't be stored or an invalidation that can't be sent. They
//! count hits, misses, loads and evictions through the OpenTelemetry metrics API; the [`metrics`]
//! module lists them.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod cache;
pub mod clock;
pub mod locker;
pub mod metrics;
pub mod settings;
pub mod worker;

pub use cache::{
    CacheEntry, CacheError, CacheKey, CacheOptions, CacheStore, CacheValue, KeyPrefix,
};
pub use settings::CacheSettings;
pub use worker::{WorkerHost, WorkerService};
