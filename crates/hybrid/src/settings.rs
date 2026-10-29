//! Settings for a hybrid deployment: its two layers, the backplane and the worker.

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
///         "backplane": { "ping_interval_ms": 2000 }
///     }"#,
/// )?;
///
/// assert_eq!(settings.local.max_ttl_secs, Some(60));
/// assert_eq!(settings.worker.reconnect_delay_ms, 100);
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
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

    /// The Redis backplane that carries invalidations between nodes.
    pub backplane: RedisBackplaneSettings,

    /// For the backplane worker the store starts.
    pub worker: BackplaneWorkerSettings,
}

impl Default for HybridCacheSettings {
    fn default() -> Self {
        Self {
            local: InMemoryCacheSettings {
                max_ttl_secs: Some(DEFAULT_LOCAL_MAX_TTL_SECS),
                ..InMemoryCacheSettings::default()
            },
            distributed: RedisCacheSettings::default(),
            backplane: RedisBackplaneSettings::default(),
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
    /// Fails when a setting in `local`, `distributed`, `backplane` or `worker` is out of range,
    /// such as a TTL over 365 days or a ping interval of 0, or when `local.common` is set, since
    /// nothing would read it.
    pub fn validate(&self) -> Result<(), CacheError> {
        check_local_common(&self.local)?;
        self.local.validate()?;
        self.distributed.validate()?;
        self.backplane.validate()?;
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

/// Settings for a [`RedisBackplane`](crate::backplane::RedisBackplane). Its channel comes from
/// the store's name, as [`RedisBackplane`](crate::backplane::RedisBackplane) says.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RedisBackplaneSettings {
    /// How often a subscription is pinged to detect a silently lost connection: 5 seconds by
    /// default. `validate` rejects `0`.
    pub ping_interval_ms: u64,

    /// How long subscribing or a ping may take before the subscription fails: 2 seconds by
    /// default. `validate` rejects `0`.
    pub ping_timeout_ms: u64,
}

impl Default for RedisBackplaneSettings {
    fn default() -> Self {
        Self {
            ping_interval_ms: 5_000,
            ping_timeout_ms: 2_000,
        }
    }
}

impl RedisBackplaneSettings {
    /// Fails when `ping_interval_ms` or `ping_timeout_ms` is 0 or over 365 days.
    pub fn validate(&self) -> Result<(), CacheError> {
        for (field, duration) in [
            ("ping_interval_ms", self.ping_interval()),
            ("ping_timeout_ms", self.ping_timeout()),
        ] {
            check_duration(field, duration, "it must be at least 1 ms")?;
        }

        Ok(())
    }

    pub fn ping_interval(&self) -> Duration {
        Duration::from_millis(self.ping_interval_ms)
    }

    pub fn ping_timeout(&self) -> Duration {
        Duration::from_millis(self.ping_timeout_ms)
    }
}

/// How a hybrid store's backplane worker waits before subscribing again, and how it tells that
/// invalidations were lost while it stayed subscribed. `connect` takes it from
/// [`HybridCacheSettings::worker`]; a store built with `new` takes it from
/// [`with_worker_settings`](crate::store::HybridCacheStore::with_worker_settings).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
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
    /// its own heartbeat for this long, the worker treats its subscription as lost. It must be
    /// longer than `heartbeat_interval_ms`.
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

    pub fn heartbeat_interval(&self) -> Duration {
        Duration::from_millis(self.heartbeat_interval_ms)
    }

    pub fn heartbeat_timeout(&self) -> Duration {
        Duration::from_millis(self.heartbeat_timeout_ms)
    }

    pub fn reconnect_delay(&self) -> Duration {
        Duration::from_millis(self.reconnect_delay_ms)
    }

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
    fn a_zero_local_cap_is_rejected() {
        let settings: HybridCacheSettings =
            serde_json::from_str(r#"{ "local": { "max_ttl_secs": 0 } }"#).unwrap();

        assert!(matches!(
            settings.validate(),
            Err(CacheError::InvalidSettings {
                field: "max_ttl_secs",
                ..
            })
        ));
    }

    #[test]
    fn local_common_settings_are_rejected() {
        let settings: HybridCacheSettings =
            serde_json::from_str(r#"{ "local": { "common": { "ttl_secs": 60 } } }"#).unwrap();

        assert!(matches!(
            settings.validate(),
            Err(CacheError::InvalidSettings {
                field: "local.common",
                ..
            })
        ));
    }

    #[test]
    fn a_zero_ping_interval_or_timeout_is_rejected() {
        assert!(HybridCacheSettings::default().validate().is_ok());

        for (field, backplane) in [
            (
                "ping_interval_ms",
                RedisBackplaneSettings {
                    ping_interval_ms: 0,
                    ..RedisBackplaneSettings::default()
                },
            ),
            (
                "ping_timeout_ms",
                RedisBackplaneSettings {
                    ping_timeout_ms: 0,
                    ..RedisBackplaneSettings::default()
                },
            ),
        ] {
            let settings = HybridCacheSettings {
                backplane,
                ..HybridCacheSettings::default()
            };

            assert!(
                matches!(settings.validate(), Err(CacheError::InvalidSettings { field: f, .. }) if f == field),
                "{field}"
            );
        }
    }

    #[test]
    fn a_backplane_or_worker_wait_over_365_days_is_rejected() {
        let year_ms = sisa_caching_core::settings::MAX_TTL_SECS * 1_000;
        let worker = |ms| BackplaneWorkerSettings {
            max_reconnect_delay_ms: ms,
            heartbeat_timeout_ms: ms,
            ..BackplaneWorkerSettings::default()
        };

        assert!(worker(year_ms).validate().is_ok());
        assert!(matches!(
            worker(year_ms + 1).validate(),
            Err(CacheError::InvalidSettings {
                field: "max_reconnect_delay_ms",
                ..
            })
        ));
        assert!(matches!(
            RedisBackplaneSettings {
                ping_interval_ms: year_ms + 1,
                ..RedisBackplaneSettings::default()
            }
            .validate(),
            Err(CacheError::InvalidSettings {
                field: "ping_interval_ms",
                ..
            })
        ));
    }

    #[test]
    fn a_zero_reconnect_delay_or_a_longest_wait_below_it_is_rejected() {
        for (field, first, longest) in [
            ("reconnect_delay_ms", 0, 5_000),
            ("max_reconnect_delay_ms", 100, 99),
            ("max_reconnect_delay_ms", 100, 0),
        ] {
            let settings = HybridCacheSettings {
                worker: BackplaneWorkerSettings {
                    reconnect_delay_ms: first,
                    max_reconnect_delay_ms: longest,
                    ..BackplaneWorkerSettings::default()
                },
                ..HybridCacheSettings::default()
            };

            assert!(
                matches!(settings.validate(), Err(CacheError::InvalidSettings { field: f, .. }) if f == field),
                "{first} and {longest}"
            );
        }
    }

    #[test]
    fn a_zero_heartbeat_interval_or_a_timeout_no_longer_than_it_is_rejected() {
        for (field, interval, timeout) in [
            ("heartbeat_interval_ms", 0, 5_000),
            ("heartbeat_timeout_ms", 1_000, 1_000),
            ("heartbeat_timeout_ms", 1_000, 500),
            ("heartbeat_timeout_ms", 1_000, 0),
        ] {
            let settings = HybridCacheSettings {
                worker: BackplaneWorkerSettings {
                    heartbeat_interval_ms: interval,
                    heartbeat_timeout_ms: timeout,
                    ..BackplaneWorkerSettings::default()
                },
                ..HybridCacheSettings::default()
            };

            assert!(
                matches!(settings.validate(), Err(CacheError::InvalidSettings { field: f, .. }) if f == field),
                "{interval} and {timeout}"
            );
        }
    }
}
