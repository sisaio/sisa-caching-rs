use core::fmt;
use std::{ops::Deref, sync::Arc};

use serde::{Deserialize, Serialize};

use crate::cache::{CacheError, cache_error::Result};

#[doc(hidden)]
pub const KEY_SEPARATOR: char = ':';

/// Joins a store's name to the keys inside it. A name never contains an empty level, so it never
/// holds `::`, and the first `::` in a backend key always ends the name: store `a` with key
/// `b:item` and store `a:b` with key `item` stay apart as `a::b:item` and `a:b::item`, whatever
/// the key holds.
#[doc(hidden)]
pub const NAME_SEPARATOR: &str = "::";

/// A store name or key prefix: one or more levels joined by `:`, none empty, such as `app` or
/// `tenant:user`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct KeyPrefix(Arc<str>);

impl KeyPrefix {
    /// Fails with [`CacheError::InvalidKey`] when `prefix` is empty or has an empty level.
    pub fn new(prefix: impl Into<Arc<str>>) -> Result<Self> {
        let prefix = prefix.into();

        Self::validate(&prefix)?;

        Ok(Self(prefix))
    }

    /// The prefix as given, such as `tenant:user`.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The name's string, shared rather than copied, for the metrics' `store` attribute.
    pub(crate) fn shared(&self) -> Arc<str> {
        Arc::clone(&self.0)
    }

    /// Checks `prefix` as `new` does, without building one.
    #[doc(hidden)]
    pub fn validate(prefix: &str) -> Result<()> {
        if prefix.split(KEY_SEPARATOR).any(str::is_empty) {
            return Err(CacheError::InvalidKey(format!(
                "store name or prefix `{prefix}` must be non-empty, without an empty level"
            )));
        }

        Ok(())
    }

    /// Whether `full_key` is under this prefix: `user` matches `user:42` but not `users:42`.
    // Inlined, as a store's `remove_prefix` calls it for every entry.
    #[inline]
    #[doc(hidden)]
    pub fn matches(&self, full_key: &str) -> bool {
        full_key
            .strip_prefix(self.as_str())
            .is_some_and(|rest| rest.starts_with(KEY_SEPARATOR))
    }

    /// The key the store named `self` writes for `key`: `app::user:42` for `user:42` in `app`.
    #[doc(hidden)]
    pub fn format_key(&self, key: &str) -> String {
        format!("{self}{NAME_SEPARATOR}{key}")
    }

    /// The start of every key the store named `self` writes: `app::` for `app`.
    #[doc(hidden)]
    pub fn format_scope(&self) -> String {
        format!("{self}{NAME_SEPARATOR}")
    }
}

impl fmt::Display for KeyPrefix {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for KeyPrefix {
    type Error = CacheError;

    fn try_from(prefix: String) -> Result<Self> {
        Self::new(prefix)
    }
}

impl From<KeyPrefix> for String {
    fn from(prefix: KeyPrefix) -> Self {
        prefix.to_string()
    }
}

/// Joins a prefix and a key into `prefix:key`, such as `user:42`. Stores take any `&str` as a key,
/// and a `CacheKey` derefs to one, so this is only a checked way to write one: the prefix has no
/// empty level and the key has no `:`.
///
/// Put the version of what the key holds in the prefix, as below, and bump it when a field is
/// renamed or changes type. Nothing checks that it's there.
///
/// ```
/// use sisa_caching_core::CacheKey;
///
/// let key = CacheKey::new("user:v1", 42)?;
///
/// assert_eq!(&*key, "user:v1:42");
/// # Ok::<(), sisa_caching_core::CacheError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CacheKey(String);

impl CacheKey {
    /// Fails with [`CacheError::InvalidKey`] when `prefix` is empty or has an empty level, or `key`
    /// is empty or holds `:`.
    pub fn new(prefix: &str, key: impl fmt::Display) -> Result<Self> {
        KeyPrefix::validate(prefix)?;

        // Formatted once, then checked: the key is what follows the prefix and its `:`.
        let full = format!("{prefix}{KEY_SEPARATOR}{key}");
        let key = &full[prefix.len() + KEY_SEPARATOR.len_utf8()..];

        if key.is_empty() || key.contains(KEY_SEPARATOR) {
            return Err(CacheError::InvalidKey(format!(
                "key `{key}` must be non-empty, without `{KEY_SEPARATOR}`"
            )));
        }

        Ok(Self(full))
    }
}

impl Deref for CacheKey {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CacheKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl From<CacheKey> for String {
    fn from(key: CacheKey) -> Self {
        key.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_is_its_prefix_and_key_joined_by_a_colon() {
        assert_eq!(
            &*CacheKey::new("tenant:user", 42).unwrap(),
            "tenant:user:42"
        );
    }

    #[test]
    fn empty_levels_and_colons_in_keys_are_rejected() {
        for prefix in [":", "user:", ":user", "a::b"] {
            assert!(KeyPrefix::new(prefix).is_err(), "prefix {prefix:?}");
            assert!(CacheKey::new(prefix, 42).is_err(), "key prefix {prefix:?}");
        }
        assert!(KeyPrefix::new("").is_err());
        assert!(CacheKey::new("", 42).is_err());
        for key in ["", "a:b"] {
            assert!(CacheKey::new("user", key).is_err(), "key {key:?}");
        }
    }

    #[test]
    fn a_prefix_matches_whole_levels_only() {
        let user = KeyPrefix::new("user").unwrap();

        assert!(user.matches("user:42"));
        assert!(user.matches("user:profile:42"));
        assert!(!user.matches("users:42"));
        assert!(!user.matches("user"));
    }

    #[test]
    fn keys_of_two_stores_never_collide() {
        let store_a = KeyPrefix::new("a").unwrap();
        let store_ab = KeyPrefix::new("a:b").unwrap();

        assert_eq!(store_a.format_key("b:item"), "a::b:item");
        assert_eq!(store_ab.format_key("item"), "a:b::item");
        // Store names never hold `::`, so the first one always ends the name.
        assert_eq!(store_a.format_key("::b"), "a::::b");
        assert_eq!(store_a.format_scope(), "a::");
    }
}
