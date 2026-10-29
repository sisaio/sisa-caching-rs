//! An in-process cache store with TTLs, an optional entry limit, and least-recently-used eviction.
//!
//! One [`InMemoryCacheStore`] holds values of every type. Name the type when you read; reading a
//! key as another type fails with `CacheError::TypeMismatch`. Clones share the same entries.
//!
//! Values keep their Rust form and are never serialized here, but they still need `Serialize`
//! and `Deserialize`, like on every store, so a store can be swapped for another without touching
//! its call sites. A type without serde, such as a compiled regex or a handle, can't be cached.
//!
//! ```
//! use std::time::Duration;
//!
//! use sisa_caching_core::{CacheOptions, CacheStore};
//! use sisa_caching_memory::{InMemoryCacheSettings, InMemoryCacheStore};
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let cache = InMemoryCacheStore::new("app")?.with_settings(
//!     InMemoryCacheSettings {
//!         max_entries: Some(10_000),
//!         ..Default::default()
//!     },
//! )?;
//!
//! cache.set("user:v1:42", "Ada".to_string(), CacheOptions::with_ttl(Duration::from_secs(60))).await?;
//!
//! assert_eq!(cache.get::<String>("user:v1:42").await?, Some("Ada".to_string()));
//! # Ok(())
//! # }
//! ```
//!
//! An expired entry reads as a miss and stays until it is overwritten, a full store trims, or a
//! purge drops it. The store purges every `purge_interval_secs` of its settings, 60 seconds by
//! default. To purge on a schedule of your own, turn that off and call `purge`:
//!
//! ```
//! use sisa_caching_memory::{InMemoryCacheSettings, InMemoryCacheStore};
//!
//! let cache = InMemoryCacheStore::new("app")?.with_settings(InMemoryCacheSettings {
//!     purge_interval_secs: None,
//!     ..Default::default()
//! })?;
//!
//! // From your own scheduler:
//! let removed = cache.purge();
//! # assert_eq!(removed, 0);
//! # Ok::<(), sisa_caching_core::CacheError>(())
//! ```

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod settings;
pub mod store;

pub use settings::InMemoryCacheSettings;
pub use store::InMemoryCacheStore;
