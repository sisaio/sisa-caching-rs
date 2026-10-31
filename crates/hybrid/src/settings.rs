//! Settings for a hybrid deployment: its two layers and the backplane worker.

use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize};
use sisa_caching_core::{
    cache::CacheError,
    settings::{CacheSettings, check_duration},
};
use sisa_caching_memory::settings::InMemoryCacheSettings;
use sisa_caching_redis::settings::RedisCacheSettings;

/// The default local `max_ttl_secs`: the longest a node serves a stale copy when a lost
/// invalidation goes unnoticed.
pub const DEFAULT_LOCAL_MAX_TTL_SECS: u64 = 60;

/// Everything a hybrid deployment on Redis is configured with, as one config section for
/// [`HybridCacheStore::connect`](crate::store::HybridCacheStore::connect):
///
/// ```
/// # use sisa_caching_hybrid::settings::HybridCacheSettings;
/// let settings: HybridCacheSettings = serde_json::from_str(
///     r#"{
///         "local": { "max_entries": 100000, "max_ttl_secs": 60, "purge_interval_secs": 60 },
///         "distributed": { "common": { "ttl_secs": 300, "lock_timeout_ms": 5000 } },
///         "worker": { "heartbeat_timeout_ms": 3000 }
///     }"#,
/// )?;
///
/// assert_eq!(settings.local.max_ttl_secs, Some(60));
/// assert_eq!(settings.worker.reconnect_delay_ms, 100);
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct HybridCacheSettings {
    /// The in-memory layer on each node. Its `max_ttl_secs` bounds how long a node can serve a
    /// stale copy when a lost invalidation goes unnoticed. It is [`DEFAULT_LOCAL_MAX_TTL_SECS`]
    /// by default, and when a config file leaves it out or sets it to null; only code can turn
    /// the cap off, with `None`. Its `common` must stay unset: hybrid calls take TTLs and locks
    /// from `distributed.common`.
    #[serde(deserialize_with = "local_settings")]
    pub local: InMemoryCacheSettings,

    /// The shared Redis store. Its `common` settings also apply to calls through the hybrid.
    pub distributed: RedisCacheSettings,

    /// The backplane worker's settings: how it subscribes again and checks heartbeats.
    pub worker: BackplaneWorkerSettings,
}

impl Default for HybridCacheSettings {
    fn default() -> Self {
        let mut local = InMemoryCacheSettings::default();
        local.max_ttl_secs = Some(DEFAULT_LOCAL_MAX_TTL_SECS);

        Self {
            local,
            distributed: RedisCacheSettings::default(),
            worker: BackplaneWorkerSettings::default(),
        }
    }
}

/// Reads `local`, keeping the default cap when the section leaves `max_ttl_secs` out.
fn local_settings<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<InMemoryCacheSettings, D::Error> {
    let mut local = InMemoryCacheSettings::deserialize(deserializer)?;
    local.max_ttl_secs.get_or_insert(DEFAULT_LOCAL_MAX_TTL_SECS);

    Ok(local)
}

impl HybridCacheSettings {
    /// Fails when a setting in `local`, `distributed` or `worker` is out of range, such as a TTL
    /// over 365 days or a heartbeat interval of 0, or when `local.common` is set, since nothing
    /// would read it.
    pub fn validate(&self) -> Result<(), CacheError> {
        check_local_common(&self.local)?;
        self.local.validate()?;
        self.distributed.validate()?;
        self.worker.validate()
    }
}

/// Fails when the local layer's `common` is set: hybrid calls read `distributed.common`.
pub(crate) fn check_local_common(local: &InMemoryCacheSettings) -> Result<(), CacheError> {
    if local.common == CacheSettings::default() {
        return Ok(());
    }

    Err(CacheError::invalid_settings(
        "local.common",
        "is ignored: hybrid calls use distributed.common; cap local copies with \
         local.max_ttl_secs",
    ))
}

/// How a hybrid store's backplane worker waits before subscribing again, and how it tells that
/// invalidations were lost while it stayed subscribed. `connect` takes it from
/// [`HybridCacheSettings::worker`]; a store built with `new` takes it from
/// [`with_worker_settings`](crate::store::HybridCacheStore::with_worker_settings).
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
    fn local_copies_are_capped_at_60_seconds_unless_set_otherwise() {
        let cap = |json| {
            serde_json::from_str::<HybridCacheSettings>(json)
                .unwrap()
                .local
                .max_ttl()
        };
        let minute = Some(Duration::from_secs(60));

        assert_eq!(HybridCacheSettings::default().local.max_ttl(), minute);
        assert_eq!(cap("{}"), minute);
        assert_eq!(cap(r#"{ "local": { "max_entries": 1000 } }"#), minute);
        assert_eq!(cap(r#"{ "local": { "max_ttl_secs": null } }"#), minute);
        assert_eq!(
            cap(r#"{ "local": { "max_ttl_secs": 5 } }"#),
            Some(Duration::from_secs(5))
        );
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
            (
                "max_ttl_secs",
                from_json(r#"{ "local": { "max_ttl_secs": 0 } }"#),
            ),
            (
                "local.common",
                from_json(r#"{ "local": { "common": { "ttl_secs": 60 } } }"#),
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
