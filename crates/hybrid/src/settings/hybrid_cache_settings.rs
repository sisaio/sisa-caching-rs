use serde::{Deserialize, Serialize};
use sisa_caching_core::{
    cache::CacheError,
    settings::{CacheSettings, NoUnknownFields},
};
use sisa_caching_memory::settings::InMemoryCacheSettings;
use sisa_caching_redis::settings::{RedisCacheSettings, RedisSettings};

use super::{BackplaneWorkerSettings, LocalSettings};

/// Everything a hybrid store is configured with, as one config section. The shared settings sit
/// at the top, beside the sections for the two layers and the worker, and an unknown field fails:
///
/// ```
/// # use sisa_caching_hybrid::settings::HybridCacheSettings;
/// let settings: HybridCacheSettings = serde_json::from_str(
///     r#"{
///         "default_ttl_secs": 300,
///         "lock_timeout_ms": 5000,
///         "local": { "ttl_secs": 60, "max_entries": 100000 },
///         "distributed": { "scan_batch_size": 500 },
///         "worker": { "heartbeat_timeout_ms": 3000 }
///     }"#,
/// )?;
///
/// assert_eq!(settings.common.default_ttl_secs, Some(300));
/// assert_eq!(settings.local.ttl_secs, 60);
/// assert_eq!(settings.worker.reconnect_delay_ms, 100);
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct HybridCacheSettings {
    /// The default TTL, refresh-ahead and the `get_or_set` lock wait of every hybrid call. A write
    /// stores its TTL in the distributed store, where an entry without one stays until removed. A
    /// node's local copy always expires: after `local.ttl_secs`, or with the entry if sooner.
    #[serde(flatten)]
    pub common: CacheSettings,

    /// The in-memory layer on each node.
    pub local: LocalSettings,

    /// The Redis store's own settings, for the one
    /// [`connect`](crate::store::HybridCacheStore::connect) builds. It holds no TTL settings: the
    /// hybrid store's own apply. A store built with [`new`](crate::store::HybridCacheStore::new)
    /// takes the distributed store as it was built, so this goes unread there.
    pub distributed: RedisSettings,

    /// The backplane worker's settings: how it subscribes again and checks heartbeats.
    pub worker: BackplaneWorkerSettings,

    // Last, so it sees only the fields no other one took, and fails on them.
    #[serde(flatten, skip_serializing)]
    pub(crate) unknown_fields: NoUnknownFields,
}

// Each part's own defaults.
impl Default for HybridCacheSettings {
    fn default() -> Self {
        Self {
            common: CacheSettings::default(),
            local: LocalSettings::default(),
            distributed: RedisSettings::default(),
            worker: BackplaneWorkerSettings::default(),
            unknown_fields: NoUnknownFields,
        }
    }
}

impl HybridCacheSettings {
    /// Fails when a setting is out of range, such as a TTL over 365 days, a local TTL of 0 or a
    /// heartbeat interval of 0.
    pub fn validate(&self) -> Result<(), CacheError> {
        self.common.validate()?;
        self.local.validate()?;
        self.distributed.validate()?;
        self.worker.validate()
    }

    /// The memory store's settings for the local layer: the hybrid's `common` and `local`.
    pub(crate) fn memory_settings(&self) -> InMemoryCacheSettings {
        let mut settings = InMemoryCacheSettings::default();
        settings.common = self.common.clone();
        settings.memory = self.local.memory.clone();

        settings
    }

    /// The Redis store's settings for the distributed layer. Its TTL settings stay unset, as
    /// hybrid calls read the hybrid store's.
    pub(crate) fn redis_settings(&self) -> RedisCacheSettings {
        let mut settings = RedisCacheSettings::default();
        settings.redis = self.distributed.clone();

        settings
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn local_copies_expire_after_60_seconds_unless_set_otherwise() {
        let read = |json| serde_json::from_str::<HybridCacheSettings>(json);
        let ttl = |json| read(json).unwrap().local.ttl();
        let minute = Duration::from_secs(60);

        assert_eq!(HybridCacheSettings::default().local.ttl(), minute);
        assert_eq!(ttl("{}"), minute);
        assert_eq!(ttl(r#"{ "local": { "max_entries": 1000 } }"#), minute);
        assert_eq!(
            ttl(r#"{ "local": { "ttl_secs": 5 } }"#),
            Duration::from_secs(5)
        );
        assert!(
            read(r#"{ "local": { "ttl_secs": null } }"#).is_err(),
            "local copies always expire"
        );
    }

    #[test]
    fn a_misspelled_or_misplaced_setting_fails_to_read() {
        for json in [
            r#"{ "default_ttl_sec": 300 }"#,
            r#"{ "ttl_secs": 300 }"#,
            r#"{ "common": { "default_ttl_secs": 300 } }"#,
            r#"{ "local": { "default_ttl_secs": 300 } }"#,
            r#"{ "distributed": { "default_ttl_secs": 300 } }"#,
            r#"{ "worker": { "default_ttl_secs": 300 } }"#,
        ] {
            assert!(
                serde_json::from_str::<HybridCacheSettings>(json).is_err(),
                "{json}"
            );
        }
    }

    #[test]
    fn out_of_range_settings_are_rejected() {
        let year_ms = sisa_caching_core::settings::MAX_TTL_SECS * 1_000;
        let from_json = |json| serde_json::from_str::<HybridCacheSettings>(json).unwrap();
        let worker = |worker| HybridCacheSettings {
            worker,
            ..HybridCacheSettings::default()
        };
        let reconnect = |reconnect_delay_ms, max_reconnect_delay_ms| {
            worker(BackplaneWorkerSettings {
                reconnect_delay_ms,
                max_reconnect_delay_ms,
                ..BackplaneWorkerSettings::default()
            })
        };
        let heartbeat = |heartbeat_interval_ms, heartbeat_timeout_ms| {
            worker(BackplaneWorkerSettings {
                heartbeat_interval_ms,
                heartbeat_timeout_ms,
                ..BackplaneWorkerSettings::default()
            })
        };

        assert!(HybridCacheSettings::default().validate().is_ok());
        assert!(reconnect(100, year_ms).validate().is_ok());
        assert!(heartbeat(1_000, year_ms).validate().is_ok());

        for (field, settings) in [
            ("ttl_secs", from_json(r#"{ "local": { "ttl_secs": 0 } }"#)),
            (
                "ttl_secs",
                from_json(r#"{ "local": { "ttl_secs": 31536001 } }"#),
            ),
            (
                "default_ttl_secs",
                from_json(r#"{ "default_ttl_secs": 0 }"#),
            ),
            (
                "max_entries",
                from_json(r#"{ "local": { "max_entries": 0 } }"#),
            ),
            (
                "scan_batch_size",
                from_json(r#"{ "distributed": { "scan_batch_size": 0 } }"#),
            ),
            ("heartbeat_interval_ms", heartbeat(year_ms + 1, year_ms + 2)),
            ("reconnect_delay_ms", reconnect(0, 5_000)),
            ("max_reconnect_delay_ms", reconnect(100, 99)),
            ("max_reconnect_delay_ms", reconnect(100, 0)),
            ("max_reconnect_delay_ms", reconnect(100, year_ms + 1)),
            ("heartbeat_interval_ms", heartbeat(0, 5_000)),
            ("heartbeat_timeout_ms", heartbeat(1_000, 1_000)),
            ("heartbeat_timeout_ms", heartbeat(1_000, 500)),
            ("heartbeat_timeout_ms", heartbeat(1_000, 0)),
        ] {
            assert!(
                matches!(settings.validate(), Err(CacheError::InvalidSettings { field: f, .. }) if f == field),
                "{field}: {settings:?}"
            );
        }
    }
}
