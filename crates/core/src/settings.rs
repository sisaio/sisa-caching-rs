//! The settings every store has, and the 365-day limit on durations.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::cache::CacheError;

/// The longest TTL, residency cap or purge interval a store accepts: 365 days.
pub const MAX_TTL_SECS: u64 = 365 * 24 * 60 * 60;

/// Settings every store has.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CacheSettings {
    /// The TTL of a write that doesn't set one. `None` keeps values until removed or evicted;
    /// `validate` rejects `0`, which would expire every write at once.
    pub ttl_secs: Option<u64>,

    /// The share of an entry's TTL after which `get_or_set` reloads it ahead of expiry, such as
    /// `0.8`. `None` turns refresh-ahead off; `validate` rejects a value outside `(0, 1)`.
    pub refresh_threshold: Option<f64>,

    /// How long `get_or_set` waits for another caller's load of the same key before it loads the
    /// value itself: 30 seconds by default. `None` waits until that load finishes, so a loader
    /// that hangs holds up every caller of its key for as long. `validate` rejects `0`, which
    /// would let every caller load at once.
    pub lock_timeout_ms: Option<u64>,
}

impl Default for CacheSettings {
    fn default() -> Self {
        Self {
            ttl_secs: None,
            refresh_threshold: None,
            lock_timeout_ms: Some(30_000),
        }
    }
}

impl CacheSettings {
    /// Fails when `ttl_secs` is 0 or over [`MAX_TTL_SECS`], `lock_timeout_ms` is 0, or
    /// `refresh_threshold` is not between 0 and 1.
    pub fn validate(&self) -> Result<(), CacheError> {
        for (field, value, none) in [
            ("ttl_secs", self.ttl_secs, "for no TTL"),
            (
                "lock_timeout_ms",
                self.lock_timeout_ms,
                "to wait without a limit",
            ),
        ] {
            if value == Some(0) {
                return Err(CacheError::InvalidSettings {
                    field,
                    reason: format!("is 0; use None {none}"),
                });
            }
        }

        if let Some(secs) = self.ttl_secs.filter(|secs| *secs > MAX_TTL_SECS) {
            return Err(CacheError::too_long("ttl_secs", secs));
        }

        match self.refresh_threshold {
            Some(share) if !(share > 0.0 && share < 1.0) => Err(CacheError::InvalidSettings {
                field: "refresh_threshold",
                reason: format!(
                    "is {share}; it is a share of the TTL between 0 and 1, such as 0.8"
                ),
            }),
            _ => Ok(()),
        }
    }

    pub fn default_ttl(&self) -> Option<Duration> {
        self.ttl_secs.map(Duration::from_secs)
    }

    pub fn lock_timeout(&self) -> Option<Duration> {
        self.lock_timeout_ms.map(Duration::from_millis)
    }

    pub fn refresh_threshold(&self) -> Option<f64> {
        self.refresh_threshold
            .filter(|share| *share > 0.0 && *share < 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn by_default_get_or_set_waits_30_seconds_for_another_load() {
        let settings = CacheSettings::default();

        assert_eq!(settings.lock_timeout(), Some(Duration::from_secs(30)));
        assert_eq!(settings.default_ttl(), None);
        assert_eq!(settings.refresh_threshold(), None);
    }

    #[test]
    fn a_default_ttl_over_365_days_is_rejected() {
        let settings = |ttl_secs| CacheSettings {
            ttl_secs: Some(ttl_secs),
            ..CacheSettings::default()
        };

        assert!(settings(MAX_TTL_SECS).validate().is_ok());
        assert!(matches!(
            settings(MAX_TTL_SECS + 1).validate(),
            Err(CacheError::InvalidSettings {
                field: "ttl_secs",
                ..
            })
        ));
    }

    #[test]
    fn a_zero_ttl_or_lock_wait_is_rejected() {
        for (field, settings) in [
            (
                "ttl_secs",
                CacheSettings {
                    ttl_secs: Some(0),
                    ..CacheSettings::default()
                },
            ),
            (
                "lock_timeout_ms",
                CacheSettings {
                    lock_timeout_ms: Some(0),
                    ..CacheSettings::default()
                },
            ),
        ] {
            assert!(
                matches!(settings.validate(), Err(CacheError::InvalidSettings { field: f, .. }) if f == field),
                "{field}"
            );
        }
    }

    #[test]
    fn a_refresh_threshold_outside_zero_and_one_is_rejected() {
        let settings = |share| CacheSettings {
            refresh_threshold: Some(share),
            ..CacheSettings::default()
        };

        assert!(settings(0.8).validate().is_ok());
        for share in [0.0, 1.0, 80.0, f64::NAN] {
            assert!(
                matches!(
                    settings(share).validate(),
                    Err(CacheError::InvalidSettings {
                        field: "refresh_threshold",
                        ..
                    })
                ),
                "{share}"
            );
        }
    }

    #[test]
    fn refresh_ahead_is_on_only_for_a_share_between_zero_and_one() {
        let threshold = |share| {
            CacheSettings {
                refresh_threshold: Some(share),
                ..CacheSettings::default()
            }
            .refresh_threshold()
        };

        assert_eq!(threshold(0.8), Some(0.8));
        for share in [0.0, 1.0, -0.5, 1.5, f64::NAN] {
            assert_eq!(threshold(share), None, "{share}");
        }
    }
}
