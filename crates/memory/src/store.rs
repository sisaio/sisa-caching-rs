//! The memory store.

use std::{
    any::{Any, type_name},
    collections::HashMap,
    hash::{BuildHasher, RandomState},
    mem,
    sync::{
        Arc, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use sisa_caching_core::{
    cache::{CacheEntry, CacheError, CacheStore, CacheValue, KeyPrefix, PurgeableStore},
    clock::{Clock, SystemClock},
    locker::Locker,
    metrics::{Layer, METRICS},
    settings::CacheSettings,
    worker::Worker,
};

use crate::{settings::InMemoryCacheSettings, worker::PurgeWorker};

/// A stored value of any type. A read asks for the type it expects and gets it back, or an error.
type Erased = Arc<dyn Any + Send + Sync>;
type EntryMap = HashMap<String, StoredEntry>;

/// The entries of a store and its clones, split over `shards` maps by key hash, plus how many
/// there are, a counter that orders their use, and a lock that lets one trim run at a time.
struct Entries {
    shards: Box<[RwLock<EntryMap>]>,
    hasher: RandomState,
    len: AtomicUsize,
    uses: AtomicU64,
    trimming: Mutex<()>,
}

impl Entries {
    fn new(shards: usize) -> Self {
        Self {
            shards: (0..shards).map(|_| RwLock::default()).collect(),
            hasher: RandomState::new(),
            len: AtomicUsize::new(0),
            uses: AtomicU64::new(0),
            trimming: Mutex::new(()),
        }
    }

    /// A number above every one handed out before, so entries order by when they were last used.
    fn next_use(&self) -> u64 {
        self.uses.fetch_add(1, Ordering::Relaxed)
    }

    /// The map that holds `key`. One map needs no hash.
    fn shard(&self, key: &str) -> &RwLock<EntryMap> {
        match self.shards.len() {
            1 => &self.shards[0],
            count => &self.shards[self.hasher.hash_one(key) as usize % count],
        }
    }
}

fn read(shard: &RwLock<EntryMap>) -> RwLockReadGuard<'_, EntryMap> {
    shard.read().unwrap_or_else(|e| e.into_inner())
}

/// Values the store lets go are freed after this guard is dropped: a value's Drop can run any
/// code, a call to this store included, which would deadlock under the lock, and freeing many
/// values would hold up every other caller.
fn write(shard: &RwLock<EntryMap>) -> RwLockWriteGuard<'_, EntryMap> {
    shard.write().unwrap_or_else(|e| e.into_inner())
}

/// An entry plus when the store lets it go, on the monotonic clock so wall-clock changes don't
/// move it, and when it was last stored or read, so a full store drops the least recently used.
struct StoredEntry {
    entry: CacheEntry<Erased>,
    deadline: Option<Instant>,
    last_used: AtomicU64,
}

impl StoredEntry {
    fn is_expired(&self, now: Instant) -> bool {
        self.deadline.is_some_and(|deadline| now >= deadline)
    }
}

/// An in-process store. One store holds values of every type; `get::<User>` and `set(&key, user,
/// None)` name the type per call, and a read with the wrong type fails with
/// [`CacheError::TypeMismatch`].
///
/// The store drops its expired entries every `purge_interval_secs` of its settings, on a task it
/// starts on its first write inside a tokio runtime. The task stops when the store's last clone
/// drops.
#[derive(Clone)]
pub struct InMemoryCacheStore {
    name: KeyPrefix,
    clock: Arc<dyn Clock>,
    settings: InMemoryCacheSettings,
    entries: Arc<Entries>,
    locker: Arc<Locker>,
    worker: Worker,
}

/// The name, the settings and how many entries it holds; never a value.
impl std::fmt::Debug for InMemoryCacheStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InMemoryCacheStore")
            .field("name", &self.name)
            .field("settings", &self.settings)
            .field("len", &self.entries.len.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl InMemoryCacheStore {
    /// A store named `name`, such as `app` or `tenant:eu`, on the system clock. Fails with
    /// [`CacheError::InvalidKey`] when the name is empty or has an empty level.
    pub fn new(name: &str) -> Result<Self, CacheError> {
        Ok(Self {
            name: KeyPrefix::new(name)?,
            clock: Arc::new(SystemClock),
            settings: InMemoryCacheSettings::default(),
            entries: Arc::new(Entries::new(1)),
            locker: Arc::default(),
            worker: Worker::new(),
        })
    }

    /// Runs the store on `clock` instead of the system clock, so tests can move time. Set it while
    /// building the store, before any write.
    pub fn with_clock(mut self, clock: impl Clock) -> Self {
        self.clock = Arc::new(clock);

        self
    }

    /// Fails when a setting is out of range, such as a TTL over 365 days, so a bad value stops
    /// setup instead of a later write. The store starts empty, split over `shards` maps.
    pub fn with_settings(mut self, settings: InMemoryCacheSettings) -> Result<Self, CacheError> {
        settings.validate()?;
        self.entries = Arc::new(Entries::new(settings.shards()));
        // New entries need a purge of their own; the store this came from keeps its own.
        self.worker = Worker::new();
        self.settings = settings;

        Ok(self)
    }

    pub fn settings(&self) -> &InMemoryCacheSettings {
        &self.settings
    }

    /// Drops expired entries, then the least recently used, down to about nine tenths of `max` so
    /// the next inserts need no work. One trim runs at a time, and one that waited for another
    /// checks the count again.
    fn trim(&self, max: usize) {
        let trimming = self
            .entries
            .trimming
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut removed = Vec::new();

        if self.entries.len.load(Ordering::Relaxed) > max {
            let now = self.clock.instant();

            removed.extend(self.extract(|_, stored| stored.is_expired(now)));

            let target = max - max / 10;
            let mut uses: Vec<u64> = Vec::new();

            for shard in &self.entries.shards {
                uses.extend(
                    read(shard)
                        .values()
                        .map(|stored| stored.last_used.load(Ordering::Relaxed)),
                );
            }

            if uses.len() > target {
                let excess = uses.len() - target;
                // Uses are unique, so `excess` entries are at or below the cutoff.
                let (_, &mut cutoff, _) = uses.select_nth_unstable(excess - 1);
                let evicted =
                    self.extract(|_, stored| stored.last_used.load(Ordering::Relaxed) <= cutoff);

                METRICS.evicted(&self.name, evicted.len());
                tracing::debug!(
                    store = %self.name,
                    evicted = evicted.len(),
                    "store full; dropped the least recently used entries"
                );
                removed.extend(evicted);
            }
        }

        // Let the next trim start, then free what this one dropped.
        drop(trimming);
        drop(removed);
    }

    /// A copy that shares the entries but not the `Worker`, for the store's purge worker, which
    /// would otherwise keep itself running after the store's last clone drops.
    pub(crate) fn for_worker(&self) -> Self {
        Self {
            worker: Worker::none(),
            ..self.clone()
        }
    }

    /// Takes out every entry `pick` chooses, one map at a time. The caller frees them.
    fn extract(&self, pick: impl Fn(&str, &StoredEntry) -> bool) -> Vec<(String, StoredEntry)> {
        let mut taken = Vec::new();

        for shard in &self.entries.shards {
            let before = taken.len();

            taken.extend(write(shard).extract_if(|key, stored| pick(key, stored)));
            self.entries
                .len
                .fetch_sub(taken.len() - before, Ordering::Relaxed);
        }

        taken
    }
}

impl PurgeableStore for InMemoryCacheStore {
    #[tracing::instrument(level = "debug", name = "memory purge", skip_all, fields(store = %self.name, removed))]
    fn purge(&self) -> usize {
        let now = self.clock.instant();
        let removed = self.extract(|_, stored| stored.is_expired(now)).len();

        tracing::Span::current().record("removed", removed);

        if removed > 0 {
            tracing::debug!(store = %self.name, removed, "expired entries removed");
        }

        removed
    }

    fn purge_interval(&self) -> Option<Duration> {
        self.settings.purge_interval()
    }
}

impl CacheStore for InMemoryCacheStore {
    type Error = CacheError;

    fn name(&self) -> &KeyPrefix {
        &self.name
    }

    fn cache_settings(&self) -> &CacheSettings {
        &self.settings.common
    }

    fn locker(&self) -> &Locker {
        &self.locker
    }

    fn clock(&self) -> &dyn Clock {
        &*self.clock
    }

    #[tracing::instrument(level = "debug", name = "memory get", skip_all, fields(key = %self.name.format_key(key)))]
    async fn get_entry<CV: CacheValue>(
        &self,
        key: &str,
    ) -> Result<Option<CacheEntry<CV>>, CacheError> {
        let now = self.clock.instant();
        // An expired entry stays until a purge, a trim or a write replaces it.
        let found = read(self.entries.shard(key))
            .get(key)
            .filter(|stored| !stored.is_expired(now))
            .map(|stored| {
                stored
                    .last_used
                    .store(self.entries.next_use(), Ordering::Relaxed);

                stored.entry.clone()
            });

        let Some(entry) = found else {
            METRICS.read(&self.name, Layer::Memory, false);

            return Ok(None);
        };

        let value = entry
            .value
            .downcast_ref::<CV>()
            .cloned()
            .ok_or(CacheError::TypeMismatch {
                expected: type_name::<CV>(),
            })?;

        METRICS.read(&self.name, Layer::Memory, true);

        Ok(Some(entry.map(|_| value)))
    }

    #[tracing::instrument(level = "debug", name = "memory set", skip_all, fields(key = %self.name.format_key(key)))]
    async fn set_entry<CV: CacheValue>(
        &self,
        key: &str,
        entry: CacheEntry<CV>,
    ) -> Result<(), CacheError> {
        // Entries only come in here, so the purge starts with the first one.
        if self.settings.purge_interval().is_some() {
            self.worker.start_once("in-memory-cache-purge-worker", || {
                PurgeWorker::new(self.for_worker()).run()
            });
        }

        let now = self.clock.instant();
        // The store holds an entry for its remaining lifetime, or `max_ttl_secs` if that is
        // shorter.
        let ttl = match (
            entry.remaining_ttl(self.clock.now()),
            self.settings.max_ttl(),
        ) {
            (Some(remaining), Some(cap)) => Some(remaining.min(cap)),
            (remaining, cap) => remaining.or(cap),
        };

        let mut entries = write(self.entries.shard(key));
        let replaced = entries.insert(
            key.to_owned(),
            StoredEntry {
                entry: entry.map(|value| Arc::new(value) as Erased),
                deadline: ttl.and_then(|ttl| now.checked_add(ttl)),
                last_used: AtomicU64::new(self.entries.next_use()),
            },
        );

        // Counted while the map is locked, so a remove never counts down before this counts up.
        if replaced.is_none() {
            self.entries.len.fetch_add(1, Ordering::Relaxed);
        }

        // Unlock, then free the value that was replaced (see `write`).
        drop(entries);
        drop(replaced);

        if let Some(max) = self.settings.max_entries()
            && self.entries.len.load(Ordering::Relaxed) > max
        {
            self.trim(max);
        }

        Ok(())
    }

    #[tracing::instrument(level = "debug", name = "memory remove", skip_all, fields(key = %self.name.format_key(key)))]
    async fn remove(&self, key: &str) -> Result<bool, CacheError> {
        let now = self.clock.instant();
        let removed = write(self.entries.shard(key)).remove(key);

        if removed.is_some() {
            self.entries.len.fetch_sub(1, Ordering::Relaxed);
        }

        // An expired entry goes too, but it wasn't there for a reader, as on Redis.
        Ok(removed.is_some_and(|stored| !stored.is_expired(now)))
    }

    #[tracing::instrument(level = "debug", name = "memory remove_prefix", skip_all, fields(store = %self.name(), %prefix))]
    async fn remove_prefix(&self, prefix: &str) -> Result<usize, CacheError> {
        let prefix = KeyPrefix::new(prefix)?;
        let now = self.clock.instant();
        let removed = self.extract(|key, _| prefix.matches(key));

        Ok(removed
            .iter()
            .filter(|(_, stored)| !stored.is_expired(now))
            .count())
    }

    #[tracing::instrument(level = "debug", name = "memory clear", skip_all, fields(store = %self.name()))]
    async fn clear(&self) -> Result<(), CacheError> {
        // Clones share the name, so every key in the maps is this store's: take each map whole.
        let cleared: Vec<EntryMap> = self
            .entries
            .shards
            .iter()
            .map(|shard| {
                let taken = mem::take(&mut *write(shard));

                self.entries.len.fetch_sub(taken.len(), Ordering::Relaxed);

                taken
            })
            .collect();

        drop(cleared);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        convert::Infallible,
        fmt::Display,
        future::pending,
        ops::Range,
        sync::{atomic::AtomicUsize, mpsc},
        time::Duration,
    };

    use futures_util::{FutureExt, future::join_all};
    use serde::{Deserialize, Serialize};
    use sisa_caching_core::{CacheKey, CacheOptions};
    use sisa_caching_testing::{ManualClock, eventually};

    use super::*;

    fn key(key: impl Display) -> CacheKey {
        CacheKey::new("user", key).unwrap()
    }

    fn ttl(secs: u64) -> CacheOptions {
        CacheOptions::with_ttl(Duration::from_secs(secs))
    }

    fn secs(secs: u64) -> Duration {
        Duration::from_secs(secs)
    }

    fn store(clock: &ManualClock, settings: InMemoryCacheSettings) -> InMemoryCacheStore {
        InMemoryCacheStore::new("app")
            .unwrap()
            .with_clock(clock.clone())
            .with_settings(settings)
            .unwrap()
    }

    fn with_ttl(ttl_secs: u64) -> InMemoryCacheSettings {
        InMemoryCacheSettings {
            common: CacheSettings {
                ttl_secs: Some(ttl_secs),
                ..CacheSettings::default()
            },
            ..InMemoryCacheSettings::default()
        }
    }

    fn with_limit(max_entries: usize) -> InMemoryCacheSettings {
        InMemoryCacheSettings {
            max_entries: Some(max_entries),
            ..InMemoryCacheSettings::default()
        }
    }

    /// The ids in `ids` the store still holds.
    async fn held(store: &InMemoryCacheStore, ids: Range<u32>) -> Vec<u32> {
        let mut held = Vec::new();

        for id in ids {
            if store.get::<u32>(&key(id)).await.unwrap().is_some() {
                held.push(id);
            }
        }

        held
    }

    #[test]
    fn a_store_name_with_an_empty_level_is_rejected() {
        assert_eq!(
            InMemoryCacheStore::new("tenant:eu")
                .unwrap()
                .name()
                .as_str(),
            "tenant:eu"
        );

        for name in ["", "app:", "a::b"] {
            assert!(
                matches!(
                    InMemoryCacheStore::new(name),
                    Err(CacheError::InvalidKey(_))
                ),
                "{name:?}"
            );
        }
    }

    #[tokio::test]
    async fn debug_shows_the_store_but_never_a_value() {
        let store = store(&ManualClock::new(), InMemoryCacheSettings::default());

        store.set(&key(1), "secret".to_owned(), None).await.unwrap();

        let shown = format!("{store:?}");

        assert!(shown.contains(r#"name: KeyPrefix("app")"#), "{shown}");
        assert!(shown.contains("len: 1"), "{shown}");
        assert!(!shown.contains("secret"), "{shown}");
    }

    #[tokio::test]
    async fn one_store_holds_values_of_many_types() {
        let store = store(&ManualClock::new(), InMemoryCacheSettings::default());

        store.set(&key(1), "ada".to_owned(), None).await.unwrap();
        store.set(&key(2), 42_u64, None).await.unwrap();

        assert_eq!(
            store.get::<String>(&key(1)).await.unwrap().as_deref(),
            Some("ada")
        );
        assert_eq!(store.get::<u64>(&key(2)).await.unwrap(), Some(42));
        assert!(matches!(
            store.get::<u64>(&key(1)).await,
            Err(CacheError::TypeMismatch { expected: "u64" })
        ));

        // Any string is a key.
        store.set("user:a:b", 3_u8, None).await.unwrap();
        assert_eq!(store.get::<u8>("user:a:b").await.unwrap(), Some(3));

        // A write replaces the value and its type.
        store.set(&key(1), 7_u64, None).await.unwrap();

        assert_eq!(store.get::<u64>(&key(1)).await.unwrap(), Some(7));
    }

    #[tokio::test]
    async fn values_expire_after_the_default_or_the_per_call_ttl() {
        let clock = ManualClock::new();
        let store = store(&clock, with_ttl(10));

        store.set(&key(1), 1_u32, None).await.unwrap();
        store.set(&key(2), 2_u32, ttl(5)).await.unwrap();

        clock.advance(secs(5));
        assert_eq!(held(&store, 1..3).await, [1]);

        clock.advance(secs(5));
        assert!(held(&store, 1..3).await.is_empty());
    }

    #[tokio::test]
    async fn a_zero_ttl_removes_the_value() {
        let store = store(&ManualClock::new(), InMemoryCacheSettings::default());

        store.set(&key(1), 1_u32, None).await.unwrap();
        store
            .set(&key(1), 1_u32, CacheOptions::with_ttl(Duration::ZERO))
            .await
            .unwrap();

        assert!(held(&store, 1..2).await.is_empty());
    }

    #[tokio::test]
    async fn a_ttl_over_365_days_is_rejected_and_nothing_is_stored() {
        let store = store(&ManualClock::new(), InMemoryCacheSettings::default());

        assert!(matches!(
            store
                .set(&key(1), 1_u32, CacheOptions::with_ttl(Duration::MAX))
                .await,
            Err(CacheError::InvalidSettings { field: "ttl", .. })
        ));
        assert!(held(&store, 1..2).await.is_empty());
    }

    #[tokio::test]
    async fn set_entry_keeps_the_entrys_own_lifetime() {
        let clock = ManualClock::new();
        let store = store(&clock, with_ttl(1));
        let forever = CacheEntry::new(1_u32, clock.now(), None);
        let created_earlier = CacheEntry::new(2_u32, clock.now() - secs(50), Some(secs(60)));

        store.set_entry(&key(1), forever.clone()).await.unwrap();
        store
            .set_entry(&key(2), created_earlier.clone())
            .await
            .unwrap();
        clock.advance(secs(9));

        assert_eq!(store.get_entry(&key(1)).await.unwrap(), Some(forever));
        assert_eq!(
            store.get_entry(&key(2)).await.unwrap(),
            Some(created_earlier)
        );

        clock.advance(secs(1));
        assert_eq!(
            held(&store, 1..3).await,
            [1],
            "the 60 s entry had 10 s left"
        );
    }

    #[tokio::test]
    async fn max_ttl_caps_how_long_an_entry_stays_but_keeps_its_metadata() {
        let clock = ManualClock::new();
        let store = store(
            &clock,
            InMemoryCacheSettings {
                max_ttl_secs: Some(10),
                ..InMemoryCacheSettings::default()
            },
        );

        store.set(&key(1), 1_u32, ttl(60)).await.unwrap();

        let entry = store.get_entry::<u32>(&key(1)).await.unwrap().unwrap();

        assert_eq!(entry.ttl(), Some(secs(60)));

        clock.advance(secs(10));
        assert!(held(&store, 1..2).await.is_empty());
    }

    #[tokio::test]
    async fn a_wall_clock_change_does_not_move_a_deadline() {
        let clock = ManualClock::new();
        let store = store(&clock, InMemoryCacheSettings::default());

        store.set(&key(1), 1_u32, ttl(10)).await.unwrap();
        clock.jump_wall_clock(secs(3600));

        assert_eq!(held(&store, 1..2).await, [1]);
    }

    #[tokio::test]
    async fn removals_touch_only_their_keys() {
        let store = store(&ManualClock::new(), InMemoryCacheSettings::default());
        let username = CacheKey::new("username", 1).unwrap();

        store.set(&key(1), 1_u32, None).await.unwrap();
        store.set(&key(2), 2_u32, None).await.unwrap();
        store.set(&username, 3_u32, None).await.unwrap();

        assert!(store.remove(&key(1)).await.unwrap());
        assert!(!store.remove(&key(1)).await.unwrap(), "already gone");
        assert_eq!(
            store.remove_prefix("user").await.unwrap(),
            1,
            "`user` does not match `username`"
        );
        assert_eq!(store.get::<u32>(&username).await.unwrap(), Some(3));

        for prefix in ["", "user:", "a::b"] {
            assert!(
                matches!(
                    store.remove_prefix(prefix).await,
                    Err(CacheError::InvalidKey(_))
                ),
                "{prefix:?}"
            );
        }

        store.clear().await.unwrap();
        assert_eq!(store.get::<u32>(&username).await.unwrap(), None);
    }

    #[tokio::test]
    async fn removals_count_only_live_entries() {
        let clock = ManualClock::new();
        let store = store(&clock, InMemoryCacheSettings::default());

        store.set(&key(1), 1_u32, ttl(1)).await.unwrap();
        store.set(&key(2), 2_u32, ttl(1)).await.unwrap();
        store.set(&key(3), 3_u32, None).await.unwrap();
        clock.advance(secs(2));

        assert!(!store.remove(&key(1)).await.unwrap(), "expired");
        assert_eq!(store.remove_prefix("user").await.unwrap(), 1);
        assert_eq!(store.purge(), 0, "the expired entries went too");
    }

    #[tokio::test]
    async fn a_versioned_prefix_drops_only_its_version() {
        let store = store(&ManualClock::new(), InMemoryCacheSettings::default());
        let v1 = CacheKey::new("user:v1", 1).unwrap();
        let v2 = CacheKey::new("user:v2", 1).unwrap();

        store.set(&v1, 1_u32, None).await.unwrap();
        store.set(&v2, 2_u32, None).await.unwrap();

        assert_eq!(store.remove_prefix("user:v1").await.unwrap(), 1);
        assert_eq!(store.get::<u32>(&v2).await.unwrap(), Some(2));

        store.set(&v1, 1_u32, None).await.unwrap();
        assert_eq!(
            store.remove_prefix("user").await.unwrap(),
            2,
            "the name drops every version"
        );
    }

    #[tokio::test]
    async fn purge_drops_the_expired_entries() {
        let clock = ManualClock::new();
        let store = store(&clock, InMemoryCacheSettings::default());

        store.set(&key(1), 1_u32, ttl(1)).await.unwrap();
        store.set(&key(2), 2_u32, ttl(1)).await.unwrap();
        store.set(&key(3), 3_u32, None).await.unwrap();
        clock.advance(secs(1));

        assert_eq!(store.purge(), 2);
        assert_eq!(held(&store, 1..4).await, [3]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_store_purges_on_its_own_from_its_first_write() {
        let clock = ManualClock::new();
        let store = store(&clock, InMemoryCacheSettings::default());

        store.set(&key(1), 1_u32, ttl(1)).await.unwrap();
        clock.advance(secs(1));
        tokio::time::sleep(secs(61)).await;

        assert_eq!(store.purge(), 0, "its own worker already purged it");
    }

    #[tokio::test]
    async fn the_purge_stops_when_the_last_clone_drops() {
        let store = store(&ManualClock::new(), InMemoryCacheSettings::default());
        let clone = store.clone();
        let entries = Arc::downgrade(&store.entries);

        store.set(&key(1), 1_u32, None).await.unwrap();
        drop(store);
        drop(clone);

        eventually("the worker drops its copy", || async {
            entries.upgrade().is_none()
        })
        .await;
    }

    #[tokio::test]
    async fn without_a_purge_interval_a_write_starts_no_worker() {
        let store = store(
            &ManualClock::new(),
            InMemoryCacheSettings {
                purge_interval_secs: None,
                ..InMemoryCacheSettings::default()
            },
        );

        store.set(&key(1), 1_u32, None).await.unwrap();

        assert_eq!(
            Arc::strong_count(&store.entries),
            1,
            "no worker holds a copy"
        );
    }

    #[test]
    fn a_write_outside_a_runtime_starts_no_purge_until_one_inside() {
        let store = store(&ManualClock::new(), InMemoryCacheSettings::default());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();

        // The memory store never waits, so its calls finish on the first poll.
        store
            .set(&key(1), 1_u32, None)
            .now_or_never()
            .unwrap()
            .unwrap();
        assert_eq!(
            Arc::strong_count(&store.entries),
            1,
            "no worker holds a copy"
        );

        runtime.block_on(store.set(&key(2), 2_u32, None)).unwrap();
        assert_eq!(Arc::strong_count(&store.entries), 2, "the worker holds one");
    }

    #[tokio::test]
    async fn a_full_store_drops_the_least_recently_used() {
        let store = store(&ManualClock::new(), with_limit(10));

        for id in 0..10_u32 {
            store.set(&key(id), id, None).await.unwrap();
        }
        // Reading 0 makes 1 and 2 the least recently used.
        store.get::<u32>(&key(0)).await.unwrap();
        store.set(&key(10), 10_u32, None).await.unwrap();

        assert_eq!(held(&store, 0..11).await, [0, 3, 4, 5, 6, 7, 8, 9, 10]);
    }

    #[tokio::test]
    async fn a_full_store_drops_expired_entries_first() {
        let clock = ManualClock::new();
        let store = store(&clock, with_limit(10));

        for id in 0..10_u32 {
            let options = (id < 5).then(|| ttl(1));

            store.set(&key(id), id, options).await.unwrap();
        }
        clock.advance(secs(1));
        store.set(&key(10), 10_u32, None).await.unwrap();

        assert_eq!(held(&store, 5..11).await, [5, 6, 7, 8, 9, 10]);
    }

    #[tokio::test]
    async fn a_full_store_trims_to_nine_tenths_of_its_limit() {
        let clock = ManualClock::new();
        let store = store(&clock, with_limit(10));

        store.set(&key(0), 0_u32, ttl(1)).await.unwrap();
        for id in 1..10_u32 {
            store.set(&key(id), id, None).await.unwrap();
        }
        clock.advance(secs(1));
        // Dropping the expired entry alone gets back to the limit, but the trim goes on to 9, so
        // the next writes need no work.
        store.set(&key(10), 10_u32, None).await.unwrap();

        assert_eq!(held(&store, 0..11).await, [2, 3, 4, 5, 6, 7, 8, 9, 10]);
    }

    #[tokio::test]
    async fn every_write_and_removal_keeps_the_entry_count_right() {
        let clock = ManualClock::new();
        let store = store(
            &clock,
            InMemoryCacheSettings {
                max_entries: Some(10),
                shards: Some(4),
                ..InMemoryCacheSettings::default()
            },
        );
        let order = |id: u32| CacheKey::new("order", id).unwrap();

        for id in 0..10_u32 {
            store
                .set_entry(&key(id), CacheEntry::new(id, clock.now(), None))
                .await
                .unwrap();
        }
        // Replaced; removed twice; removed by a zero TTL twice: 8 left.
        store.set(&key(0), 0_u32, None).await.unwrap();
        store.remove(&key(1)).await.unwrap();
        store.remove(&key(1)).await.unwrap();
        for _ in 0..2 {
            store
                .set(&key(2), 2_u32, CacheOptions::with_ttl(Duration::ZERO))
                .await
                .unwrap();
        }
        // Expired, then found by a read and by a purge: 6 left.
        store.set(&key(3), 3_u32, ttl(1)).await.unwrap();
        store.set(&key(4), 4_u32, ttl(1)).await.unwrap();
        clock.advance(secs(1));
        store.get::<u32>(&key(3)).await.unwrap();
        store.purge();
        // Added, then removed by prefix: 6 left.
        store.set(&order(1), 1_u32, None).await.unwrap();
        store.set(&order(2), 2_u32, None).await.unwrap();
        store.remove_prefix("order").await.unwrap();

        for id in 10..14_u32 {
            store.set(&key(id), id, None).await.unwrap();
        }
        assert_eq!(
            held(&store, 0..14).await.len(),
            10,
            "at the limit, nothing goes"
        );

        store.set(&key(14), 14_u32, None).await.unwrap();
        assert_eq!(
            held(&store, 0..15).await.len(),
            9,
            "past it, trimmed to nine tenths"
        );

        store.clear().await.unwrap();
        for id in 0..10_u32 {
            store.set(&key(id), id, None).await.unwrap();
        }
        assert_eq!(
            held(&store, 0..10).await.len(),
            10,
            "a clear counts down to zero"
        );
    }

    #[tokio::test]
    async fn get_or_set_loads_a_missing_value_once_for_every_caller() {
        let store = store(&ManualClock::new(), InMemoryCacheSettings::default());
        let loads = AtomicUsize::new(0);
        let load = || async {
            loads.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(10)).await;

            Ok::<_, Infallible>("ada".to_owned())
        };
        let user = key(1);

        let values = join_all((0..10).map(|_| store.get_or_set(&user, load, None))).await;

        assert!(values.iter().all(|value| value.as_deref() == Ok("ada")));
        assert_eq!(loads.load(Ordering::SeqCst), 1);
        assert_eq!(
            store.get::<String>(&user).await.unwrap().as_deref(),
            Some("ada")
        );
    }

    #[tokio::test]
    async fn get_or_set_stops_waiting_for_another_load_after_the_lock_timeout() {
        let store = store(
            &ManualClock::new(),
            InMemoryCacheSettings {
                common: CacheSettings {
                    lock_timeout_ms: Some(20),
                    ..CacheSettings::default()
                },
                ..InMemoryCacheSettings::default()
            },
        );
        // The first load never finishes, so it holds the key's lock for good.
        let user = key(1);
        let mut stuck =
            Box::pin(store.get_or_set(&user, pending::<Result<String, Infallible>>, None));

        assert!((&mut stuck).now_or_never().is_none());

        let loaded = tokio::time::timeout(
            secs(1),
            store.get_or_set(
                &key(1),
                || async { Ok::<_, Infallible>("ada".to_owned()) },
                None,
            ),
        )
        .await
        .expect("the lock timeout ends the wait");

        assert_eq!(loaded.as_deref(), Ok("ada"));
    }

    #[tokio::test]
    async fn get_or_set_stores_over_a_value_of_another_type() {
        let store = store(&ManualClock::new(), InMemoryCacheSettings::default());

        store.set(&key(1), 42_u64, None).await.unwrap();

        let loaded = store
            .get_or_set(
                &key(1),
                || async { Ok::<_, Infallible>("ada".to_owned()) },
                None,
            )
            .await;

        assert_eq!(loaded.as_deref(), Ok("ada"));
        assert_eq!(
            store.get::<String>(&key(1)).await.unwrap().as_deref(),
            Some("ada")
        );
    }

    /// The fields of every event at the info level or above, as production logs would carry them.
    #[derive(Clone, Default)]
    struct InfoLogs(Arc<Mutex<Vec<String>>>);

    impl tracing::Subscriber for InfoLogs {
        fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
            *metadata.level() <= tracing::Level::INFO
        }

        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

        fn event(&self, event: &tracing::Event<'_>) {
            struct Fields<'a>(&'a mut Vec<String>);

            impl tracing::field::Visit for Fields<'_> {
                fn record_debug(
                    &mut self,
                    field: &tracing::field::Field,
                    value: &dyn std::fmt::Debug,
                ) {
                    self.0.push(format!("{field}={value:?}"));
                }
            }

            event.record(&mut Fields(&mut self.0.lock().unwrap()));
        }

        fn enter(&self, _: &tracing::span::Id) {}

        fn exit(&self, _: &tracing::span::Id) {}
    }

    #[tokio::test]
    async fn warnings_name_the_store_but_never_the_key() {
        let logs = InfoLogs::default();
        let _logging = tracing::subscriber::set_default(logs.clone());
        let store = store(&ManualClock::new(), InMemoryCacheSettings::default());
        let email = key("ada@example.com");

        // Another type is there, so `get_or_set` warns that it won't cache what it loaded.
        store.set(&email, 42_u64, None).await.unwrap();
        store
            .get_or_set(
                &email,
                || async { Ok::<_, Infallible>("ada".to_owned()) },
                None,
            )
            .await
            .unwrap();

        let logged = logs.0.lock().unwrap().join(" ");

        assert!(logged.contains("store=app"), "{logged}");
        assert!(!logged.contains("ada@example.com"), "{logged}");
    }

    #[tokio::test]
    async fn get_or_set_refreshes_ahead_and_keeps_the_old_value_if_that_fails() {
        let clock = ManualClock::new();
        let store = store(
            &clock,
            InMemoryCacheSettings {
                common: CacheSettings {
                    ttl_secs: Some(10),
                    refresh_threshold: Some(0.8),
                    ..CacheSettings::default()
                },
                ..InMemoryCacheSettings::default()
            },
        );
        let user = key(1);
        let get_or_set = |loaded: Result<&'static str, &'static str>| {
            store.get_or_set(
                &user,
                move || async move { loaded.map(str::to_owned) },
                None,
            )
        };

        store.set(&key(1), "old".to_owned(), None).await.unwrap();

        clock.advance(secs(7));
        assert_eq!(
            get_or_set(Ok("new")).await.as_deref(),
            Ok("old"),
            "not due yet"
        );

        clock.advance(secs(1));
        assert_eq!(
            get_or_set(Err("down")).await.as_deref(),
            Ok("old"),
            "failed refresh"
        );
        assert_eq!(
            get_or_set(Ok("new")).await.as_deref(),
            Ok("new"),
            "refreshed"
        );
        assert_eq!(
            store.get::<String>(&user).await.unwrap().as_deref(),
            Some("new")
        );
    }

    #[tokio::test]
    async fn a_failed_refresh_that_outlasts_the_ttl_returns_the_error() {
        let clock = ManualClock::new();
        let store = store(
            &clock,
            InMemoryCacheSettings {
                common: CacheSettings {
                    ttl_secs: Some(10),
                    refresh_threshold: Some(0.8),
                    ..CacheSettings::default()
                },
                ..InMemoryCacheSettings::default()
            },
        );

        store.set(&key(1), "old".to_owned(), None).await.unwrap();
        clock.advance(secs(9));

        // The loader runs past second 10, when the cached value expires.
        let loaded = store
            .get_or_set(
                &key(1),
                || async {
                    clock.advance(secs(4));
                    Err::<String, _>("down")
                },
                None,
            )
            .await;

        assert_eq!(loaded, Err("down"));
    }

    /// Calls its store from `clone` and `drop`, as a value holding a handle to its own cache
    /// might. A store that cloned or freed values under its lock would deadlock on it.
    #[derive(Serialize, Deserialize)]
    struct CallsTheStore {
        #[serde(skip)]
        store: Option<InMemoryCacheStore>,
    }

    impl Clone for CallsTheStore {
        fn clone(&self) -> Self {
            if let Some(store) = &self.store {
                store.purge();
            }

            Self {
                store: self.store.clone(),
            }
        }
    }

    impl Drop for CallsTheStore {
        fn drop(&mut self) {
            if let Some(store) = &self.store {
                store.purge();
            }
        }
    }

    #[test]
    fn a_value_may_call_the_store_from_its_clone_or_drop() {
        let clock = ManualClock::new();
        let store = store(
            &clock,
            InMemoryCacheSettings {
                max_entries: Some(2),
                shards: Some(4),
                ..InMemoryCacheSettings::default()
            },
        );
        let (finished, done) = mpsc::channel();

        std::thread::spawn(move || {
            let run = |call: &mut dyn FnMut() -> Result<(), CacheError>| call().unwrap();
            let value = || CallsTheStore {
                store: Some(store.clone()),
            };
            let set = |id: u32, options: Option<CacheOptions>| {
                store
                    .set(&key(id), value(), options)
                    .now_or_never()
                    .unwrap()
            };

            // Read, replaced, removed, and replaced by an entry with no time left.
            run(&mut || set(1, None));
            run(&mut || {
                store
                    .get::<CallsTheStore>(&key(1))
                    .now_or_never()
                    .unwrap()
                    .map(drop)
            });
            run(&mut || set(1, None));
            run(&mut || store.remove(&key(1)).now_or_never().unwrap().map(drop));
            run(&mut || set(1, None));
            run(&mut || set(1, Some(CacheOptions::with_ttl(Duration::ZERO))));
            // Expired, then found by a read and by a purge.
            run(&mut || set(2, Some(ttl(1))));
            clock.advance(secs(1));
            run(&mut || {
                store
                    .get::<CallsTheStore>(&key(2))
                    .now_or_never()
                    .unwrap()
                    .map(drop)
            });
            run(&mut || set(2, Some(ttl(1))));
            clock.advance(secs(1));
            store.purge();
            // Past the limit of 2: the expired entry goes first, then the least recently used.
            run(&mut || set(1, Some(ttl(1))));
            clock.advance(secs(1));
            for id in 2..5_u32 {
                run(&mut || set(id, None));
            }
            // Removed by prefix, then cleared.
            run(&mut || {
                store
                    .remove_prefix("user")
                    .now_or_never()
                    .unwrap()
                    .map(drop)
            });
            run(&mut || set(1, None));
            run(&mut || store.clear().now_or_never().unwrap());

            finished.send(()).unwrap();
        });

        done.recv_timeout(Duration::from_secs(5))
            .expect("a value called the store while the store held its lock");
    }
}
