/// Errors every store can return.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CacheError {
    /// A store name or prefix is empty or has an empty level, or the key given to
    /// [`CacheKey::new`](crate::CacheKey::new) is empty or contains `:`.
    #[error("invalid cache key: {0}")]
    InvalidKey(String),

    /// The memory store has a value of another Rust type under the key, such as `get::<User>` on
    /// a key that `set` filled with an `Order`. The Redis store reports JSON that doesn't fit the
    /// type as its own JSON error instead.
    #[error("cached value is not a `{expected}`")]
    #[non_exhaustive]
    TypeMismatch {
        /// The requested type's name.
        expected: &'static str,
    },

    /// A setting or a write's `ttl` is out of range, such as a TTL over
    /// [`MAX_TTL_SECS`](crate::settings::MAX_TTL_SECS) or a `refresh_threshold` of `80`.
    #[error("`{field}` {reason}")]
    #[non_exhaustive]
    InvalidSettings {
        /// The setting, such as `ttl_secs`, or `ttl` for a write's options.
        field: &'static str,
        /// What is wrong with its value.
        reason: String,
    },
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

/// A `Result` whose error is a [`CacheError`].
pub type Result<T> = std::result::Result<T, CacheError>;
