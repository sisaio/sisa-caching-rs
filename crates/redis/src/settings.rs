//! Settings for the Redis store.

use serde::{Deserialize, Serialize};
use sisa_caching_core::{cache::CacheError, settings::CacheSettings};

/// The default [`RedisCacheSettings::scan_batch_size`].
pub const DEFAULT_SCAN_BATCH_SIZE: u32 = 1000;

/// Settings for a [`RedisCacheStore`](crate::store::RedisCacheStore). In a config file the
/// shared settings sit under `common`, and an unknown field fails, so a misspelled setting can't
/// silently keep its default:
///
/// ```
/// # use sisa_caching_redis::settings::RedisCacheSettings;
/// let settings: RedisCacheSettings =
///     serde_json::from_str(r#"{ "common": { "ttl_secs": 300 }, "scan_batch_size": 500 }"#)
///         .unwrap();
///
/// assert_eq!(settings.common.ttl_secs, Some(300));
/// assert!(serde_json::from_str::<RedisCacheSettings>(r#"{ "ttl_sec": 300 }"#).is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RedisCacheSettings {
    /// The settings every store has: the default TTL, refresh-ahead and the `get_or_set` lock wait.
    pub common: CacheSettings,

    /// How many keys each `SCAN` asks for while `remove_prefix` or `clear` walks the store's
    /// keys. Redis treats it as a hint and may return more or fewer. A larger batch means fewer
    /// round trips but more server work per call. 0 counts as 1.
    pub scan_batch_size: u32,
}

impl Default for RedisCacheSettings {
    fn default() -> Self {
        Self {
            common: CacheSettings::default(),
            scan_batch_size: DEFAULT_SCAN_BATCH_SIZE,
        }
    }
}

impl RedisCacheSettings {
    /// Fails when a shared setting is out of range, as
    /// [`CacheSettings::validate`](sisa_caching_core::settings::CacheSettings::validate) says.
    pub fn validate(&self) -> Result<(), CacheError> {
        self.common.validate()
    }

    pub fn scan_batch_size(&self) -> u32 {
        self.scan_batch_size.max(1)
    }
}
