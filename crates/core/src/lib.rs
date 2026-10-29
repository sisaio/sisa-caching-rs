//! The shared parts of Sisa Caching: keys, entries, settings, the [`CacheStore`] trait every store
//! implements, and the [`Worker`](worker::Worker) that runs a store's background task.
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
//! span as an error. Stores warn only about the errors they swallow, naming the store but never a
//! key, such as an invalidation that can't be sent; `get_or_set` warns at the first of a run of
//! failed store calls, and logs the success that ends it at info. A store's background task logs
//! its start and stop at info, and a panic at error. Stores count hits, misses, loads, failed store
//! calls and evictions through the OpenTelemetry metrics API; the [`metrics`] module lists them.

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
