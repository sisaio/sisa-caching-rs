//! The settings every store has, and the 365-day limit on durations.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::cache::CacheError;

/// The longest any duration setting, or a write's TTL, may be: 365 days.
pub const MAX_TTL_SECS: u64 = 365 * 24 * 60 * 60;

/// Settings every store has.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
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
    /// Fails when `ttl_secs` or `lock_timeout_ms` is 0 or over [`MAX_TTL_SECS`], or
    /// `refresh_threshold` is not between 0 and 1.
    pub fn validate(&self) -> Result<(), CacheError> {
        check_duration("ttl_secs", self.default_ttl(), "use None for no TTL")?;
        check_duration(
            "lock_timeout_ms",
            self.lock_timeout(),
            "use None to wait without a limit",
        )?;

        match self.refresh_threshold {
            Some(threshold) if !(threshold > 0.0 && threshold < 1.0) => {
                Err(CacheError::InvalidSettings {
                    field: "refresh_threshold",
                    reason: format!(
                        "is {threshold}; it is a share of the TTL between 0 and 1, such as 0.8"
                    ),
                })
            }
            _ => Ok(()),
        }
    }

    /// `ttl_secs` as a `Duration`, or `None` when a write without a TTL never expires.
    pub fn default_ttl(&self) -> Option<Duration> {
        self.ttl_secs.map(Duration::from_secs)
    }

    /// `lock_timeout_ms` as a `Duration`, or `None` to wait as long as the other load takes.
    pub fn lock_timeout(&self) -> Option<Duration> {
        self.lock_timeout_ms.map(Duration::from_millis)
    }

    /// `refresh_threshold` when refresh-ahead is on, or `None` when it is off. A value outside
    /// `(0, 1)`, which `validate` rejects, reads as off.
    pub fn refresh_threshold(&self) -> Option<f64> {
        self.refresh_threshold
            .filter(|threshold| *threshold > 0.0 && *threshold < 1.0)
    }
}

/// Fails when a duration setting is 0, with `if_zero` as the fix, or over [`MAX_TTL_SECS`]. A
/// setting that `None` turns off passes `None` through.
#[doc(hidden)]
pub fn check_duration(
    field: &'static str,
    duration: impl Into<Option<Duration>>,
    if_zero: &str,
) -> Result<(), CacheError> {
    match duration.into() {
        Some(duration) if duration.is_zero() => Err(CacheError::invalid_settings(
            field,
            format!("is 0; {if_zero}"),
        )),
        Some(duration) if duration > Duration::from_secs(MAX_TTL_SECS) => {
            Err(CacheError::too_long(field, duration))
        }
        _ => Ok(()),
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
    fn out_of_range_settings_are_rejected() {
        let ttl = |ttl_secs| CacheSettings {
            ttl_secs: Some(ttl_secs),
            ..CacheSettings::default()
        };
        let wait = |lock_timeout_ms| CacheSettings {
            lock_timeout_ms: Some(lock_timeout_ms),
            ..CacheSettings::default()
        };
        let refresh = |threshold| CacheSettings {
            refresh_threshold: Some(threshold),
            ..CacheSettings::default()
        };
        let year_ms = MAX_TTL_SECS * 1_000;

        for settings in [ttl(MAX_TTL_SECS), wait(year_ms), refresh(0.8)] {
            assert!(settings.validate().is_ok(), "{settings:?}");
        }
        for (field, settings) in [
            ("ttl_secs", ttl(0)),
            ("ttl_secs", ttl(MAX_TTL_SECS + 1)),
            ("lock_timeout_ms", wait(0)),
            ("lock_timeout_ms", wait(year_ms + 1)),
            ("refresh_threshold", refresh(0.0)),
            ("refresh_threshold", refresh(1.0)),
            ("refresh_threshold", refresh(80.0)),
            ("refresh_threshold", refresh(f64::NAN)),
        ] {
            assert!(
                matches!(settings.validate(), Err(CacheError::InvalidSettings { field: f, .. }) if f == field),
                "{settings:?}"
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
