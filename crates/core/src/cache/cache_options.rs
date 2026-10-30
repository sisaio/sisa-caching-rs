use std::time::Duration;

use crate::{cache::CacheError, settings::MAX_TTL_SECS};

/// Per-call options for a write.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct CacheOptions {
    ttl: Option<Duration>,
}

impl CacheOptions {
    /// A write that expires after `ttl` instead of the store's default TTL. Passing `None` for the
    /// options keeps the default; there is no per-call "never expire" when the store has one.
    pub fn with_ttl(ttl: Duration) -> Self {
        Self { ttl: Some(ttl) }
    }

    /// This write's TTL, or `None` to use the store's default.
    pub fn ttl(&self) -> Option<Duration> {
        self.ttl
    }

    /// Fails when `ttl` is over [`MAX_TTL_SECS`], the same limit the stores' settings have.
    pub fn validate(&self) -> Result<(), CacheError> {
        match self.ttl {
            Some(ttl) if ttl > Duration::from_secs(MAX_TTL_SECS) => {
                Err(CacheError::too_long("ttl", ttl))
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ttl_over_365_days_is_rejected() {
        let year = Duration::from_secs(MAX_TTL_SECS);

        assert!(CacheOptions::default().validate().is_ok());
        assert!(CacheOptions::with_ttl(year).validate().is_ok());

        for ttl in [year + Duration::from_secs(1), Duration::MAX] {
            assert!(matches!(
                CacheOptions::with_ttl(ttl).validate(),
                Err(CacheError::InvalidSettings { field: "ttl", .. })
            ));
        }
    }
}
