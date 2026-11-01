use std::time::Duration;

use serde::{Deserialize, Serialize};
use sisa_caching_core::{cache::CacheError, settings::check_duration};

/// How a hybrid store's backplane worker waits before subscribing again, and how it tells that
/// invalidations were lost while it stayed subscribed, as
/// [`HybridCacheSettings::worker`](super::HybridCacheSettings::worker).
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

// Retries from 100 ms backing off to 5 s, and a heartbeat every second, lost after 5 s.
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

    /// `reconnect_delay_ms` as a `Duration`.
    pub fn reconnect_delay(&self) -> Duration {
        Duration::from_millis(self.reconnect_delay_ms)
    }

    /// `max_reconnect_delay_ms` as a `Duration`.
    pub fn max_reconnect_delay(&self) -> Duration {
        Duration::from_millis(self.max_reconnect_delay_ms)
    }

    /// `heartbeat_interval_ms` as a `Duration`.
    pub fn heartbeat_interval(&self) -> Duration {
        Duration::from_millis(self.heartbeat_interval_ms)
    }

    /// `heartbeat_timeout_ms` as a `Duration`.
    pub fn heartbeat_timeout(&self) -> Duration {
        Duration::from_millis(self.heartbeat_timeout_ms)
    }
}
