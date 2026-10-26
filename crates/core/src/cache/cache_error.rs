/// Errors every store can return.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CacheError {
    /// A store name or prefix is empty or has an empty level, or the key given to
    /// [`CacheKey::new`](crate::CacheKey::new) is empty or contains `:`.
    #[error("invalid cache key: {0}")]
    InvalidKey(String),

    /// A typed in-memory value was stored as another Rust type than the one requested, such as
    /// `get::<User>` on a key that `set` filled with an `Order`. Encoded stores validate their
    /// serialized data through the codec instead.
    #[error("cached value is not a `{expected}`")]
    TypeMismatch { expected: &'static str },

    /// A setting or a write's `ttl` is out of range, such as a TTL over
    /// [`MAX_TTL_SECS`](crate::settings::MAX_TTL_SECS) or a `refresh_threshold` of `80`.
    #[error("`{field}` {reason}")]
    InvalidSettings { field: &'static str, reason: String },
}

impl CacheError {
    /// A duration over [`MAX_TTL_SECS`](crate::settings::MAX_TTL_SECS).
    #[doc(hidden)]
    pub fn too_long(field: &'static str, secs: u64) -> Self {
        Self::InvalidSettings {
            field,
            reason: format!("is {secs} seconds; the most a store accepts is 365 days"),
        }
    }
}

pub type Result<T> = std::result::Result<T, CacheError>;
