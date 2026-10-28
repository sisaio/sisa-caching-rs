//! Settings for the memory store.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use sisa_caching_core::{
    cache::CacheError,
    settings::{CacheSettings, MAX_TTL_SECS},
};

/// Settings for an [`InMemoryCacheStore`](crate::store::InMemoryCacheStore). In a config file the
/// shared settings sit under `common`, and an unknown field fails, so a misspelled setting can't
/// silently keep its default:
///
/// ```
/// # use sisa_caching_memory::settings::InMemoryCacheSettings;
/// let settings: InMemoryCacheSettings =
///     serde_json::from_str(r#"{ "common": { "ttl_secs": 300 }, "max_entries": 10000 }"#)
///         .unwrap();
///
/// assert_eq!(settings.common.ttl_secs, Some(300));
/// assert!(serde_json::from_str::<InMemoryCacheSettings>(r#"{ "ttl_sec": 300 }"#).is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InMemoryCacheSettings {
    /// The settings every store has: the default TTL, refresh-ahead and the `get_or_set` lock wait.
    pub common: CacheSettings,

    /// How often the store drops its expired entries: 60 seconds by default, from its first
    /// write. `None` or `0` turns that off, for an application that calls `purge` itself.
    pub purge_interval_secs: Option<u64>,

    /// The most entries the store holds. Past it, an insert drops expired entries, then the least
    /// recently used, down to about nine tenths of the limit. Inserts from several threads can go a
    /// few over it until that trim finishes. `None` or `0` sets no limit.
    pub max_entries: Option<usize>,

    /// The longest an entry stays in the store, whatever its TTL. The entry keeps its own creation
    /// time and TTL; only how long the store holds it is capped. `None` or `0` sets no cap.
    pub max_ttl_secs: Option<u64>,

    /// How many maps the entries are split over, each with its own lock. More maps let threads
    /// using different keys run side by side, and a `remove_prefix`, purge or trim holds up one
    /// map at a time instead of the whole store; each call then hashes its key once more.
    /// `None` or `0` means one map.
    pub shards: Option<usize>,
}

impl Default for InMemoryCacheSettings {
    fn default() -> Self {
        Self {
            common: CacheSettings::default(),
            purge_interval_secs: Some(60),
            max_entries: None,
            max_ttl_secs: None,
            shards: None,
        }
    }
}

impl InMemoryCacheSettings {
    /// Fails when `purge_interval_secs` or `max_ttl_secs` is over [`MAX_TTL_SECS`], or a shared
    /// setting is out of range, as [`CacheSettings::validate`] says.
    pub fn validate(&self) -> Result<(), CacheError> {
        self.common.validate()?;

        for (field, secs) in [
            ("purge_interval_secs", self.purge_interval_secs),
            ("max_ttl_secs", self.max_ttl_secs),
        ] {
            if let Some(secs) = secs.filter(|secs| *secs > MAX_TTL_SECS) {
                return Err(CacheError::too_long(field, secs));
            }
        }

        Ok(())
    }

    pub fn purge_interval(&self) -> Option<Duration> {
        self.purge_interval_secs
            .filter(|secs| *secs > 0)
            .map(Duration::from_secs)
    }

    pub fn max_entries(&self) -> Option<usize> {
        self.max_entries.filter(|max| *max > 0)
    }

    pub fn shards(&self) -> usize {
        self.shards.filter(|shards| *shards > 0).unwrap_or(1)
    }

    pub fn max_ttl(&self) -> Option<Duration> {
        self.max_ttl_secs
            .filter(|secs| *secs > 0)
            .map(Duration::from_secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_over_365_days_are_rejected() {
        let too_long = Some(MAX_TTL_SECS + 1);

        for (field, settings) in [
            (
                "ttl_secs",
                InMemoryCacheSettings {
                    common: CacheSettings {
                        ttl_secs: too_long,
                        ..CacheSettings::default()
                    },
                    ..InMemoryCacheSettings::default()
                },
            ),
            (
                "purge_interval_secs",
                InMemoryCacheSettings {
                    purge_interval_secs: too_long,
                    ..InMemoryCacheSettings::default()
                },
            ),
            (
                "max_ttl_secs",
                InMemoryCacheSettings {
                    max_ttl_secs: too_long,
                    ..InMemoryCacheSettings::default()
                },
            ),
        ] {
            assert!(
                matches!(settings.validate(), Err(CacheError::InvalidSettings { field: f, .. }) if f == field),
                "{field}"
            );
        }

        let year = Some(MAX_TTL_SECS);
        let settings = InMemoryCacheSettings {
            common: CacheSettings {
                ttl_secs: year,
                ..CacheSettings::default()
            },
            purge_interval_secs: year,
            max_ttl_secs: year,
            ..InMemoryCacheSettings::default()
        };

        assert!(settings.validate().is_ok(), "exactly 365 days is allowed");
    }

    #[test]
    fn by_default_purging_runs_every_minute_and_nothing_is_capped() {
        let settings = InMemoryCacheSettings::default();

        assert_eq!(settings.purge_interval(), Some(Duration::from_secs(60)));
        assert_eq!(settings.max_entries(), None);
        assert_eq!(settings.max_ttl(), None);
    }

    #[test]
    fn zero_turns_a_limit_off() {
        let settings = InMemoryCacheSettings {
            purge_interval_secs: Some(0),
            max_entries: Some(0),
            max_ttl_secs: Some(0),
            shards: Some(0),
            ..InMemoryCacheSettings::default()
        };

        assert_eq!(settings.purge_interval(), None);
        assert_eq!(settings.max_entries(), None);
        assert_eq!(settings.max_ttl(), None);
        assert_eq!(settings.shards(), 1);
    }
}
