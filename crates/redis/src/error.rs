//! The Redis store's errors.

use sisa_caching_core::cache::CacheError;

/// Errors from Redis-backed cache operations: the shared [`CacheError`]s, plus Redis and JSON
/// failures.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RedisCacheError {
    /// Shared cache errors, such as a `CacheOptions` TTL over 365 days.
    #[error(transparent)]
    Cache(#[from] CacheError),

    /// Connection, command or protocol errors from Redis.
    #[error(transparent)]
    Redis(#[from] redis::RedisError),

    /// A value couldn't be encoded to, or decoded from, JSON. Decoding fails when the stored JSON
    /// no longer fits the type, such as after a field was added without `#[serde(default)]`.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, RedisCacheError>;
