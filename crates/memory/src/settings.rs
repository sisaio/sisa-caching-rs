//! Settings for the memory store.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use sisa_caching_core::{
    cache::CacheError,
    settings::{CacheSettings, NoUnknownFields, check_duration},
};

/// The most maps a store's entries can be split over.
const MAX_SHARDS: usize = 1_024;

/// The memory store's own settings, apart from the shared ones. A hybrid store's local layer
/// takes these too, under `local`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct MemorySettings {
    /// How often the store drops its expired entries: 60 seconds by default. `None` turns that
    /// off, for an application that calls `purge` itself; `validate` rejects `0`.
    pub purge_interval_secs: Option<u64>,

    /// The most entries the store holds. Past it, an insert starts a trim that drops expired
    /// entries, then the least recently used, down to about nine tenths of the limit. Inside a
    /// tokio runtime the trim runs on the blocking pool and inserts go on meanwhile, so the store
    /// can go over the limit while it runs. `None`, the default, sets no limit; `validate` rejects
    /// `0`.
    pub max_entries: Option<usize>,

    /// How many maps the entries are split over, each with its own lock: 16 by default. More maps
    /// let threads using different keys run side by side, and a `remove_prefix`, purge or trim
    /// holds up one map at a time instead of the whole store; each call then hashes its key once
    /// more. `None` means one map, a little faster for a store used from one thread; `validate`
    /// rejects `0` and more than 1,024.
    pub shards: Option<usize>,
}

// A purge every minute, no limit, and 16 maps.
impl Default for MemorySettings {
    fn default() -> Self {
        Self {
            purge_interval_secs: Some(60),
            max_entries: None,
            shards: Some(16),
        }
    }
}

impl MemorySettings {
    /// Fails when a setting is 0, `purge_interval_secs` is over 365 days or `shards` is over
    /// 1,024.
    pub fn validate(&self) -> Result<(), CacheError> {
        check_duration(
            "purge_interval_secs",
            self.purge_interval(),
            "use None to turn purging off",
        )?;

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

    /// `purge_interval_secs` as a `Duration`, or `None` when the store doesn't purge by itself.
    pub fn purge_interval(&self) -> Option<Duration> {
        self.purge_interval_secs.map(Duration::from_secs)
    }

    /// How many maps `shards` asks for: 1 for `None`.
    pub fn shards(&self) -> usize {
        self.shards.unwrap_or(1)
    }
}

/// Settings for an [`InMemoryCacheStore`](crate::store::InMemoryCacheStore): the shared ones and
/// the memory store's own. In a config file both sit side by side, and an unknown field fails, so
/// a misspelled setting can't silently keep its default:
///
/// ```
/// # use sisa_caching_memory::settings::InMemoryCacheSettings;
/// let settings: InMemoryCacheSettings =
///     serde_json::from_str(r#"{ "default_ttl_secs": 300, "max_entries": 10000 }"#).unwrap();
///
/// assert_eq!(settings.common.default_ttl_secs, Some(300));
/// assert_eq!(settings.memory.max_entries, Some(10_000));
/// let misspelled = r#"{ "default_ttl_sec": 300 }"#;
/// assert!(serde_json::from_str::<InMemoryCacheSettings>(misspelled).is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct InMemoryCacheSettings {
    /// The settings every store has: the default TTL, refresh-ahead and the `get_or_set` lock wait.
    #[serde(flatten)]
    pub common: CacheSettings,

    /// The memory store's own settings: its purge, its limit and its maps.
    #[serde(flatten)]
    pub memory: MemorySettings,

    // Last, so it sees only the fields no other one took, and fails on them.
    #[serde(flatten, skip_serializing)]
    pub(crate) unknown_fields: NoUnknownFields,
}

// Every setting at its default, as `MemorySettings` and `CacheSettings` give them.
impl Default for InMemoryCacheSettings {
    fn default() -> Self {
        Self {
            common: CacheSettings::default(),
            memory: MemorySettings::default(),
            unknown_fields: NoUnknownFields,
        }
    }
}

impl InMemoryCacheSettings {
    /// Fails when a setting is out of range, as [`MemorySettings::validate`] and
    /// [`CacheSettings::validate`] say.
    pub fn validate(&self) -> Result<(), CacheError> {
        self.common.validate()?;
        self.memory.validate()
    }
}

#[cfg(test)]
mod tests {
    use sisa_caching_core::settings::MAX_TTL_SECS;

    use super::*;

    #[test]
    fn out_of_range_settings_are_rejected() {
        let too_long = Some(MAX_TTL_SECS + 1);
        let memory = |memory| InMemoryCacheSettings {
            memory,
            ..InMemoryCacheSettings::default()
        };

        for (field, settings) in [
            (
                "default_ttl_secs",
                InMemoryCacheSettings {
                    common: {
                        let mut settings = CacheSettings::default();
                        settings.default_ttl_secs = too_long;
                        settings
                    },
                    ..InMemoryCacheSettings::default()
                },
            ),
            (
                "purge_interval_secs",
                memory(MemorySettings {
                    purge_interval_secs: too_long,
                    ..MemorySettings::default()
                }),
            ),
            (
                "purge_interval_secs",
                memory(MemorySettings {
                    purge_interval_secs: Some(0),
                    ..MemorySettings::default()
                }),
            ),
            (
                "max_entries",
                memory(MemorySettings {
                    max_entries: Some(0),
                    ..MemorySettings::default()
                }),
            ),
            (
                "shards",
                memory(MemorySettings {
                    shards: Some(0),
                    ..MemorySettings::default()
                }),
            ),
            (
                "shards",
                memory(MemorySettings {
                    shards: Some(MAX_SHARDS + 1),
                    ..MemorySettings::default()
                }),
            ),
        ] {
            assert!(
                matches!(settings.validate(), Err(CacheError::InvalidSettings { field: rejected, .. }) if rejected == field),
                "{settings:?}"
            );
        }

        let year = Some(MAX_TTL_SECS);
        let settings = InMemoryCacheSettings {
            common: {
                let mut settings = CacheSettings::default();
                settings.default_ttl_secs = year;
                settings
            },
            memory: MemorySettings {
                purge_interval_secs: year,
                shards: Some(MAX_SHARDS),
                ..MemorySettings::default()
            },
            ..InMemoryCacheSettings::default()
        };

        assert!(
            settings.validate().is_ok(),
            "exactly 365 days and 1,024 maps are allowed"
        );
    }

    #[test]
    fn by_default_purging_runs_every_minute_nothing_is_capped_and_there_are_16_maps() {
        let settings = InMemoryCacheSettings::default();

        assert_eq!(settings.common, CacheSettings::default());
        assert_eq!(
            settings.memory.purge_interval(),
            Some(Duration::from_secs(60))
        );
        assert_eq!(settings.memory.max_entries, None);
        assert_eq!(settings.memory.shards(), 16);
    }

    #[test]
    fn none_turns_a_limit_off() {
        let settings = MemorySettings {
            purge_interval_secs: None,
            max_entries: None,
            shards: None,
        };

        assert!(settings.validate().is_ok());
        assert_eq!(settings.purge_interval(), None);
        assert_eq!(settings.max_entries, None);
        assert_eq!(settings.shards(), 1);
    }
}
