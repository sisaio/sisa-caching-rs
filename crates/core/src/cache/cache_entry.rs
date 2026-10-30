use std::time::{Duration, SystemTime};

/// A value with its creation time and TTL. Every store, memory, Redis and hybrid, reads and writes
/// this one type, so `get_entry` from one store and `set_entry` into another copies an entry as it
/// is. Its creation time and TTL can't be changed; build a new entry instead.
///
/// ```
/// use std::time::{Duration, SystemTime};
/// use sisa_caching_core::CacheEntry;
///
/// let created_at = SystemTime::UNIX_EPOCH;
/// let entry = CacheEntry::new("Ada", created_at, Some(Duration::from_secs(60)));
///
/// assert_eq!(entry.expires_at(), Some(created_at + Duration::from_secs(60)));
/// assert!(!entry.is_expired(created_at + Duration::from_secs(59)));
/// assert!(entry.is_expired(created_at + Duration::from_secs(60)));
/// ```
///
/// The TTL can't be changed in place:
///
/// ```compile_fail
/// # use std::time::{Duration, SystemTime};
/// # use sisa_caching_core::CacheEntry;
/// let mut entry = CacheEntry::new(7, SystemTime::UNIX_EPOCH, None);
/// entry.ttl = Some(Duration::from_secs(10));
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct CacheEntry<CV> {
    /// The cached value. Unlike the creation time and TTL, it can be taken out or changed in place.
    pub value: CV,
    created_at: SystemTime,
    ttl: Option<Duration>,
    expires_at: Option<SystemTime>,
}

impl<CV> CacheEntry<CV> {
    /// An entry that expires `ttl` after `created_at`, or never with `None` or a TTL so long the
    /// deadline can't be represented.
    pub fn new(value: CV, created_at: SystemTime, ttl: Option<Duration>) -> Self {
        Self {
            value,
            created_at,
            ttl,
            expires_at: ttl.and_then(|ttl| created_at.checked_add(ttl)),
        }
    }

    /// Replaces the expiry derived from `created_at + ttl`, for a store whose server tracks the
    /// deadline itself. `None` means the entry no longer expires.
    pub fn with_expires_at(mut self, expires_at: Option<SystemTime>) -> Self {
        self.expires_at = expires_at;

        self
    }

    /// When the value was written. Copying the entry to another store keeps it.
    pub fn created_at(&self) -> SystemTime {
        self.created_at
    }

    /// The TTL the entry was written with, or `None` if it had none. Refresh-ahead counts from it;
    /// when the entry expires is [`expires_at`](Self::expires_at), which a store's server can set
    /// without a TTL.
    pub fn ttl(&self) -> Option<Duration> {
        self.ttl
    }

    /// When the entry expires: `created_at + ttl`, or the deadline a store's server keeps, as
    /// [`with_expires_at`](Self::with_expires_at) sets. `None` if it never expires.
    pub fn expires_at(&self) -> Option<SystemTime> {
        self.expires_at
    }

    /// The time left before the entry expires, or `None` if it never does.
    pub fn remaining_ttl(&self, now: SystemTime) -> Option<Duration> {
        self.expires_at
            .map(|expires_at| expires_at.duration_since(now).unwrap_or_default())
    }

    /// Whether `now` is at or past [`expires_at`](Self::expires_at). An entry without one never
    /// expires.
    pub fn is_expired(&self, now: SystemTime) -> bool {
        self.expires_at.is_some_and(|expires_at| now >= expires_at)
    }

    /// Whether `threshold` of the TTL, such as `0.8`, has passed since creation. An entry without a
    /// TTL is never due.
    pub fn is_refresh_due(&self, now: SystemTime, threshold: f64) -> bool {
        self.ttl
            .and_then(|ttl| Duration::try_from_secs_f64(ttl.as_secs_f64() * threshold).ok())
            .and_then(|due| self.created_at.checked_add(due))
            .is_some_and(|due| now >= due)
    }

    /// The same entry around another value, keeping its creation time, TTL and expiry.
    pub fn map<U>(self, convert: impl FnOnce(CV) -> U) -> CacheEntry<U> {
        CacheEntry {
            value: convert(self.value),
            created_at: self.created_at,
            ttl: self.ttl,
            expires_at: self.expires_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CREATED: SystemTime = SystemTime::UNIX_EPOCH;

    fn secs(secs: u64) -> Duration {
        Duration::from_secs(secs)
    }

    #[test]
    fn an_entry_expires_its_ttl_after_creation() {
        let entry = CacheEntry::new("ada", CREATED, Some(secs(10)));

        assert!(!entry.is_expired(CREATED + secs(9)));
        assert!(entry.is_expired(CREATED + secs(10)));
        assert_eq!(entry.remaining_ttl(CREATED + secs(4)), Some(secs(6)));
        assert_eq!(
            entry.remaining_ttl(CREATED + secs(20)),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn an_entry_without_a_ttl_never_expires_or_needs_a_refresh() {
        let entry = CacheEntry::new("ada", CREATED, None);
        let much_later = CREATED + secs(1_000_000);

        assert!(!entry.is_expired(much_later));
        assert!(!entry.is_refresh_due(much_later, 0.5));
        assert_eq!(entry.remaining_ttl(much_later), None);
    }

    #[test]
    fn a_refresh_is_due_once_the_share_of_the_ttl_has_passed() {
        let entry = CacheEntry::new("ada", CREATED, Some(secs(10)));

        assert!(!entry.is_refresh_due(CREATED + Duration::from_millis(7_999), 0.8));
        assert!(entry.is_refresh_due(CREATED + secs(8), 0.8));
    }

    #[test]
    fn a_ttl_past_the_end_of_time_never_expires() {
        let entry = CacheEntry::new("ada", CREATED, Some(Duration::MAX));

        assert_eq!(entry.expires_at(), None);
        assert!(!entry.is_expired(CREATED + secs(1_000_000)));
        assert!(!entry.is_refresh_due(CREATED + secs(1_000_000), 2.0));
    }

    #[test]
    fn a_server_deadline_replaces_the_derived_one_and_survives_map() {
        let deadline = CREATED + secs(10);
        let entry = CacheEntry::new(7, CREATED, Some(secs(60)))
            .with_expires_at(Some(deadline))
            .map(|n| n.to_string());

        assert_eq!(entry.value, "7");
        assert_eq!(entry.created_at(), CREATED);
        assert_eq!(entry.ttl(), Some(secs(60)));
        assert!(entry.is_expired(deadline));
    }
}
