//! Settings for a hybrid deployment: its own TTLs, its two layers and the backplane worker.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use sisa_caching_core::{
    cache::CacheError,
    settings::{CacheSettings, NoUnknownFields, check_duration},
};
use sisa_caching_memory::settings::{InMemoryCacheSettings, MemorySettings};
use sisa_caching_redis::settings::{RedisCacheSettings, RedisSettings};

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

/// The in-memory layer of a hybrid store: how long its copies live, and the memory store's own
/// settings beside it. Its copies always expire: after `ttl_secs`, or when the entry they copy
/// does, if that is sooner.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct LocalSettings {
    /// How long a node keeps a local copy: 60 seconds by default. A copy never outlives its
    /// entry, so one with a shorter TTL left expires with it. It is also the longest a node serves
    /// a stale copy when an invalidation is lost and nothing notices, so local copies can't be
    /// kept for good, even of an entry the distributed store keeps until removed. `validate`
    /// rejects `0`.
    pub ttl_secs: u64,

    /// The memory store's own settings for the copies: `max_entries`, `purge_interval_secs` and
    /// `shards`, which sit beside `ttl_secs` in a config file.
    #[serde(flatten)]
    pub memory: MemorySettings,

    // Last, so it sees only the fields no other one took, and fails on them.
    #[serde(flatten, skip_serializing)]
    pub(crate) unknown_fields: NoUnknownFields,
}

// A minute bounds how long a lost invalidation leaves a node serving a stale copy.
impl Default for LocalSettings {
    fn default() -> Self {
        Self {
            ttl_secs: 60,
            memory: MemorySettings::default(),
            unknown_fields: NoUnknownFields,
        }
    }
}

impl LocalSettings {
    /// Fails when `ttl_secs` is 0 or over 365 days, or a memory setting is out of range, as
    /// [`MemorySettings::validate`] says.
    pub fn validate(&self) -> Result<(), CacheError> {
        check_duration(
            "ttl_secs",
            self.ttl(),
            "local copies need a TTL of at least 1 second",
        )?;

        self.memory.validate()
    }

    /// `ttl_secs` as a `Duration`.
    pub fn ttl(&self) -> Duration {
        Duration::from_secs(self.ttl_secs)
    }
}

/// How a hybrid store's backplane worker waits before subscribing again, and how it tells that
/// invalidations were lost while it stayed subscribed, as [`HybridCacheSettings::worker`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct BackplaneWorkerSettings {
    /// The first wait before subscribing again after a lost subscription: 100 ms by default. It
    /// doubles after each failure. `validate` rejects `0`.
    pub reconnect_delay_ms: u64,

    /// The longest wait between tries: 5 seconds by default. A subscription must also stay up
    /// this long before the next loss starts the wait over from `reconnect_delay_ms`. It must be
    /// at least `reconnect_delay_ms`.
    pub max_reconnect_delay_ms: u64,

    /// How often the worker publishes a heartbeat with the last sequence number its store has
    /// finished publishing: 1 second by default. A node that missed one of the store's
    /// invalidations clears its local layer at the next heartbeat. `validate` rejects `0`.
    pub heartbeat_interval_ms: u64,

    /// How long without a heartbeat from another node before this node clears its local layer, as
    /// that node may have stopped between a write and its publish: 5 seconds by default. Without
    /// its own heartbeat for this long, the worker treats its subscription as lost. Subscribing
    /// must also finish within it, and behind Sentinel the worker asks for the primary this often.
    /// It must be longer than `heartbeat_interval_ms`.
    pub heartbeat_timeout_ms: u64,
}

impl Default for BackplaneWorkerSettings {
    fn default() -> Self {
        Self {
            reconnect_delay_ms: 100,
            max_reconnect_delay_ms: 5_000,
            heartbeat_interval_ms: 1_000,
            heartbeat_timeout_ms: 5_000,
        }
    }
}

impl BackplaneWorkerSettings {
    /// Fails when a setting is 0 or over 365 days, `max_reconnect_delay_ms` is shorter than
    /// `reconnect_delay_ms`, or `heartbeat_timeout_ms` isn't longer than `heartbeat_interval_ms`,
    /// since every node would then miss heartbeats that are on time.
    pub fn validate(&self) -> Result<(), CacheError> {
        for (field, duration) in [
            ("reconnect_delay_ms", self.reconnect_delay()),
            ("max_reconnect_delay_ms", self.max_reconnect_delay()),
            ("heartbeat_interval_ms", self.heartbeat_interval()),
            ("heartbeat_timeout_ms", self.heartbeat_timeout()),
        ] {
            check_duration(field, duration, "it must be at least 1 ms")?;
        }
        if self.max_reconnect_delay_ms < self.reconnect_delay_ms {
            return Err(CacheError::invalid_settings(
                "max_reconnect_delay_ms",
                "must be at least reconnect_delay_ms",
            ));
        }
        if self.heartbeat_timeout_ms <= self.heartbeat_interval_ms {
            return Err(CacheError::invalid_settings(
                "heartbeat_timeout_ms",
                "must be longer than heartbeat_interval_ms",
            ));
        }

        Ok(())
    }

    /// `heartbeat_interval_ms` as a `Duration`.
    pub fn heartbeat_interval(&self) -> Duration {
        Duration::from_millis(self.heartbeat_interval_ms)
    }

    /// `heartbeat_timeout_ms` as a `Duration`.
    pub fn heartbeat_timeout(&self) -> Duration {
        Duration::from_millis(self.heartbeat_timeout_ms)
    }

    /// `reconnect_delay_ms` as a `Duration`.
    pub fn reconnect_delay(&self) -> Duration {
        Duration::from_millis(self.reconnect_delay_ms)
    }

    /// `max_reconnect_delay_ms` as a `Duration`.
    pub fn max_reconnect_delay(&self) -> Duration {
        Duration::from_millis(self.max_reconnect_delay_ms)
    }
}

#[cfg(test)]
mod tests {
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
