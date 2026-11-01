use std::time::Duration;

use serde::{Deserialize, Serialize};
use sisa_caching_core::{
    cache::CacheError,
    settings::{NoUnknownFields, check_duration},
};
use sisa_caching_memory::settings::MemorySettings;

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
