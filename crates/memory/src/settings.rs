//! Settings for the memory store.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use sisa_caching_core::{
    cache::CacheError,
    settings::{CacheSettings, check_duration},
};

/// The most maps a store's entries can be split over.
const MAX_SHARDS: usize = 1_024;

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

    /// How often the store drops its expired entries: 60 seconds by default. `None` turns that
    /// off, for an application that calls `purge` itself; `validate` rejects `0`.
    pub purge_interval_secs: Option<u64>,

    /// The most entries the store holds. Past it, an insert starts a trim that drops expired
    /// entries, then the least recently used, down to about nine tenths of the limit. Inside a
    /// tokio runtime the trim runs on the blocking pool and inserts go on meanwhile, so the store
    /// can go over the limit while it runs. `None` sets no limit; `validate` rejects `0`.
    pub max_entries: Option<usize>,

    /// The longest an entry stays in the store, whatever its TTL. The entry keeps its own creation
    /// time and TTL; only how long the store holds it is capped. `None` sets no cap; `validate`
    /// rejects `0`.
    pub max_ttl_secs: Option<u64>,

    /// How many maps the entries are split over, each with its own lock. More maps let threads
    /// using different keys run side by side, and a `remove_prefix`, purge or trim holds up one
    /// map at a time instead of the whole store; each call then hashes its key once more.
    /// `None` means one map; `validate` rejects `0` and more than 1,024.
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
    /// Fails when a setting is 0, `purge_interval_secs` or `max_ttl_secs` is over 365 days,
    /// `shards` is over 1,024, or a shared setting is out of range, as
    /// [`CacheSettings::validate`] says.
    pub fn validate(&self) -> Result<(), CacheError> {
        self.common.validate()?;
        check_duration(
            "purge_interval_secs",
            self.purge_interval(),
            "use None to turn purging off",
        )?;
        check_duration("max_ttl_secs", self.max_ttl(), "use None for no cap")?;

        for (field, count, none) in [
            ("max_entries", self.max_entries, "for no limit"),
            ("shards", self.shards, "for one map"),
        ] {
            if count == Some(0) {
                return Err(CacheError::invalid_settings(
                    field,
                    format!("is 0; use None {none}"),
                ));
            }
        }

        if let Some(shards) = self.shards.filter(|shards| *shards > MAX_SHARDS) {
            return Err(CacheError::invalid_settings(
                "shards",
                format!("is {shards}; the most is {MAX_SHARDS}"),
            ));
        }

        Ok(())
    }

    pub fn purge_interval(&self) -> Option<Duration> {
        self.purge_interval_secs.map(Duration::from_secs)
    }

    pub fn max_entries(&self) -> Option<usize> {
        self.max_entries
    }

    pub fn shards(&self) -> usize {
        self.shards.unwrap_or(1)
    }

    pub fn max_ttl(&self) -> Option<Duration> {
        self.max_ttl_secs.map(Duration::from_secs)
    }
}

#[cfg(test)]
mod tests {
    use sisa_caching_core::settings::MAX_TTL_SECS;

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
    fn none_turns_a_limit_off() {
        let settings = InMemoryCacheSettings {
            purge_interval_secs: None,
            max_entries: None,
            max_ttl_secs: None,
            shards: None,
            ..InMemoryCacheSettings::default()
        };

        assert!(settings.validate().is_ok());
        assert_eq!(settings.purge_interval(), None);
        assert_eq!(settings.max_entries(), None);
        assert_eq!(settings.max_ttl(), None);
        assert_eq!(settings.shards(), 1);
    }

    #[test]
    fn zero_is_rejected() {
        for (field, settings) in [
            (
                "purge_interval_secs",
                InMemoryCacheSettings {
                    purge_interval_secs: Some(0),
                    ..InMemoryCacheSettings::default()
                },
            ),
            (
                "max_entries",
                InMemoryCacheSettings {
                    max_entries: Some(0),
                    ..InMemoryCacheSettings::default()
                },
            ),
            (
                "max_ttl_secs",
                InMemoryCacheSettings {
                    max_ttl_secs: Some(0),
                    ..InMemoryCacheSettings::default()
                },
            ),
            (
                "shards",
                InMemoryCacheSettings {
                    shards: Some(0),
                    ..InMemoryCacheSettings::default()
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
    fn more_than_1024_shards_are_rejected() {
        let shards = |shards| InMemoryCacheSettings {
            shards: Some(shards),
            ..InMemoryCacheSettings::default()
        };

        assert!(shards(MAX_SHARDS).validate().is_ok());
        assert!(matches!(
            shards(MAX_SHARDS + 1).validate(),
            Err(CacheError::InvalidSettings {
                field: "shards",
                ..
            })
        ));
    }
}
