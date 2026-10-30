//! The Redis store's errors.

use serde_json::error::Category;
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
    /// no longer fits the type, such as after a field was added without `#[serde(default)]`. The
    /// message says where, but leaves serde's own message, which can quote the data, to the
    /// error's [`source`](std::error::Error::source).
    #[error("{}", json_message(.0))]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, RedisCacheError>;

/// What went wrong with the JSON, without quoting it.
fn json_message(error: &serde_json::Error) -> String {
    let (line, column) = (error.line(), error.column());

    // Only a decoding error has a position.
    if line == 0 {
        return "the value couldn't be encoded as JSON".to_owned();
    }

    match error.classify() {
        Category::Data => {
            format!("the stored JSON doesn't fit the requested type at line {line} column {column}")
        }
        _ => format!("the stored data isn't valid JSON at line {line} column {column}"),
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;

    #[test]
    fn a_decode_error_says_where_without_quoting_the_data() {
        let decoded = serde_json::from_str::<u64>(r#""ada@example.com""#).unwrap_err();
        let error = RedisCacheError::from(decoded);
        let source = error.source().unwrap().to_string();

        assert_eq!(
            error.to_string(),
            "the stored JSON doesn't fit the requested type at line 1 column 17"
        );
        assert!(source.contains("ada@example.com"), "{source}");
    }
}
