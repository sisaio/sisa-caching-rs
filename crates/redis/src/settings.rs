//! Settings for the Redis store.

use serde::{Deserialize, Serialize};
use sisa_caching_core::{
    cache::CacheError,
    settings::{CacheSettings, NoUnknownFields},
};

/// The Redis store's own settings, apart from the shared ones. A hybrid store's Redis layer takes
/// these too, under `distributed`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct RedisSettings {
    /// How many keys each `SCAN` asks for while `remove_prefix` or `clear` walks the store's
    /// keys: 1,000 by default. Redis treats it as a hint and may return more or fewer. A larger
    /// batch means fewer round trips but more server work per call. `validate` rejects `0`.
    pub scan_batch_size: u32,
}

// A hundred times Redis's own `COUNT` default of 10, so a walk takes few round trips.
impl Default for RedisSettings {
    fn default() -> Self {
        Self {
            scan_batch_size: 1_000,
        }
    }
}

impl RedisSettings {
    /// Fails when `scan_batch_size` is 0.
    pub fn validate(&self) -> Result<(), CacheError> {
        if self.scan_batch_size == 0 {
            return Err(CacheError::invalid_settings(
                "scan_batch_size",
                "is 0; it must be at least 1",
            ));
        }

        Ok(())
    }
}

/// Settings for a [`RedisCacheStore`](crate::store::RedisCacheStore): the shared ones and the
/// Redis store's own. In a config file both sit side by side, and an unknown field fails, so a
/// misspelled setting can't silently keep its default:
///
/// ```
/// # use sisa_caching_redis::settings::RedisCacheSettings;
/// let settings: RedisCacheSettings =
///     serde_json::from_str(r#"{ "default_ttl_secs": 300, "scan_batch_size": 500 }"#).unwrap();
///
/// assert_eq!(settings.common.default_ttl_secs, Some(300));
/// assert_eq!(settings.redis.scan_batch_size, 500);
/// let misspelled = r#"{ "default_ttl_sec": 300 }"#;
/// assert!(serde_json::from_str::<RedisCacheSettings>(misspelled).is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct RedisCacheSettings {
    /// The settings every store has: the default TTL, refresh-ahead and the `get_or_set` lock wait.
    #[serde(flatten)]
    pub common: CacheSettings,

    /// The Redis store's own settings: how it walks keys.
    #[serde(flatten)]
    pub redis: RedisSettings,

    // Last, so it sees only the fields no other one took, and fails on them.
    #[serde(flatten, skip_serializing)]
    pub(crate) unknown_fields: NoUnknownFields,
}

// Every setting at its default, as `RedisSettings` and `CacheSettings` give them.
impl Default for RedisCacheSettings {
    fn default() -> Self {
        Self {
            common: CacheSettings::default(),
            redis: RedisSettings::default(),
            unknown_fields: NoUnknownFields,
        }
    }
}

impl RedisCacheSettings {
    /// Fails when a setting is out of range, as [`RedisSettings::validate`] and
    /// [`CacheSettings::validate`](sisa_caching_core::settings::CacheSettings::validate) say.
    pub fn validate(&self) -> Result<(), CacheError> {
        self.common.validate()?;
        self.redis.validate()
    }
}

#[cfg(test)]
mod tests {
    use sisa_caching_core::settings::MAX_TTL_SECS;

    use super::*;

    #[test]
    fn out_of_range_settings_are_rejected() {
        let batch = |scan_batch_size| RedisCacheSettings {
            redis: RedisSettings { scan_batch_size },
            ..RedisCacheSettings::default()
        };
        let ttl = RedisCacheSettings {
            common: {
                let mut settings = CacheSettings::default();
                settings.default_ttl_secs = Some(MAX_TTL_SECS + 1);
                settings
            },
            ..RedisCacheSettings::default()
        };

        assert!(batch(1).validate().is_ok());
        for (field, settings) in [("scan_batch_size", batch(0)), ("default_ttl_secs", ttl)] {
            assert!(
                matches!(settings.validate(), Err(CacheError::InvalidSettings { field: rejected, .. }) if rejected == field),
                "{field}"
            );
        }
    }
}
