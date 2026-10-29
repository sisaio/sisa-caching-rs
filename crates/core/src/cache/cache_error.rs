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
    #[non_exhaustive]
    TypeMismatch { expected: &'static str },

    /// A setting or a write's `ttl` is out of range, such as a TTL over
    /// [`MAX_TTL_SECS`](crate::settings::MAX_TTL_SECS) or a `refresh_threshold` of `80`.
    #[error("`{field}` {reason}")]
    #[non_exhaustive]
    InvalidSettings { field: &'static str, reason: String },
}

impl CacheError {
    /// A value read as `expected` that was stored as another type.
    #[doc(hidden)]
    pub fn type_mismatch(expected: &'static str) -> Self {
        Self::TypeMismatch { expected }
    }

    /// A setting that is out of range, and why.
    #[doc(hidden)]
    pub fn invalid_settings(field: &'static str, reason: impl Into<String>) -> Self {
        Self::InvalidSettings {
            field,
            reason: reason.into(),
        }
    }

    /// A duration over [`MAX_TTL_SECS`](crate::settings::MAX_TTL_SECS).
    #[doc(hidden)]
    pub fn too_long(field: &'static str, duration: std::time::Duration) -> Self {
        Self::InvalidSettings {
            field,
            reason: format!("is {duration:?}; the most a store accepts is 365 days"),
        }
    }
}

pub type Result<T> = std::result::Result<T, CacheError>;
