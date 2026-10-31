//! The shared parts of Sisa Caching: keys, entries, settings, and the [`CacheStore`] trait every
//! store implements.
//!
//! Applications depend on `sisa-caching`, which re-exports this crate's types, or on this crate
//! and the one store crate they use. `CacheStore` is sealed: the memory, Redis and hybrid stores
//! are its only implementations, and code generic over it takes any of them. Every store has the
//! same calls:
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
//! Stores trace through [`tracing`](https://docs.rs/tracing), with keys only in debug spans, and
//! warn only about the errors they handle themselves. They count hits, misses, loads, failed store
//! calls and evictions through the OpenTelemetry metrics API, as the [`metrics`] module lists.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]
#![warn(missing_docs)]

pub mod cache;
pub mod clock;
// The per-key locks under `get_or_set`: public for the store crates, not part of the API.
#[doc(hidden)]
pub mod locker;
pub mod metrics;
pub mod settings;
// The task each store runs for itself: public for the store crates, not part of the API.
#[doc(hidden)]
pub mod worker;

// Seals `CacheStore`, so it can gain methods without a breaking release: public for the store
// crates and their tests, not part of the API.
#[doc(hidden)]
pub mod sealed {
    /// Implemented by each store this workspace ships.
    pub trait Sealed {}
}

pub use cache::{
    CacheEntry, CacheError, CacheKey, CacheOptions, CacheStore, CacheValue, KeyPrefix,
};
pub use settings::CacheSettings;
