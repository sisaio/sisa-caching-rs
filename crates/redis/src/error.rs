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
    /// message says where when serde knows, but leaves serde's own message, which can quote the
    /// data, to the error's [`source`](std::error::Error::source).
    #[error("{}", json_message(.0))]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, RedisCacheError>;

/// What went wrong with the JSON, without quoting it.
fn json_message(error: &serde_json::Error) -> String {
    let (line, column) = (error.line(), error.column());

    // An encoding error has no position, and neither has a decoding error that the type raises
    // itself, such as an untagged enum that no variant fits.
    if line == 0 {
        return "the value couldn't be encoded as JSON, or the stored JSON doesn't fit the requested type"
            .to_owned();
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
    fn a_json_error_says_where_it_can_without_quoting_the_data() {
        // An untagged enum raises its own error when no variant fits, and serde gives that no
        // position.
        #[derive(Debug, serde::Deserialize)]
        #[serde(untagged)]
        enum Null {
            Null,
        }

        let decoded = serde_json::from_str::<u64>(r#""ada@example.com""#).unwrap_err();
        let error = RedisCacheError::from(decoded);
        let source = error.source().unwrap().to_string();

        assert_eq!(
            error.to_string(),
            "the stored JSON doesn't fit the requested type at line 1 column 17"
        );
        assert!(source.contains("ada@example.com"), "{source}");

        let unplaced = serde_json::from_str::<Null>(r#""ada@example.com""#).unwrap_err();

        assert_eq!(
            RedisCacheError::from(unplaced).to_string(),
            "the value couldn't be encoded as JSON, or the stored JSON doesn't fit the requested type"
        );
    }
}
