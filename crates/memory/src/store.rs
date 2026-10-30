//! The memory store.

use std::{
    any::{Any, type_name},
    collections::HashMap,
    mem,
    sync::{
        Arc, RwLock, RwLockReadGuard, RwLockWriteGuard,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use sisa_caching_core::{
    cache::{CacheEntry, CacheError, CacheStore, CacheValue, KeyPrefix},
    clock::{Clock, SystemClock},
    locker::Locker,
    metrics::{Layer, METRICS, Reads},
    settings::CacheSettings,
    worker::Worker,
};

use crate::settings::InMemoryCacheSettings;

/// A stored value of any type. A read asks for the type it expects and gets it back, or an error.
type Erased = Arc<dyn Any + Send + Sync>;
type EntryMap = HashMap<String, StoredEntry>;

/// The entries of a store and its clones, split over `shards` maps by key hash, plus how many
/// there are, a count of writes that orders their use, and whether a trim is running, as one runs
/// at a time.
struct Entries {
    shards: Box<[RwLock<EntryMap>]>,
    len: AtomicUsize,
    uses: AtomicU64,
    trimming: AtomicBool,
}

impl Entries {
    fn new(shards: usize) -> Self {
        Self {
            shards: (0..shards).map(|_| RwLock::default()).collect(),
            len: AtomicUsize::new(0),
            uses: AtomicU64::new(0),
            trimming: AtomicBool::new(false),
        }
    }

    /// A write's number, above every one before it. A read takes the last write's number, so
    /// readers don't all update one counter, and entries read since the same write tie.
    fn next_use(&self) -> u64 {
        self.uses.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// The map that holds `key`. One map needs no hash. Each map hashes its keys with SipHash,
    /// which hostile keys can't make collide, so picking the map only has to spread keys, and a
    /// multiply per eight bytes does that for far less than a second SipHash. At worst, keys made
    /// to land on one map crowd it, like every key in a store with one map.
    fn shard(&self, key: &str) -> &RwLock<EntryMap> {
        let count = self.shards.len();

        if count == 1 {
            return &self.shards[0];
        }

        // FxHash's step, as in rustc-hash 1.
        let hash = key.as_bytes().chunks(8).fold(0_u64, |hash, chunk| {
            let mut word = [0; 8];

            word[..chunk.len()].copy_from_slice(chunk);
            (hash.rotate_left(5) ^ u64::from_le_bytes(word)).wrapping_mul(0x517c_c1b7_2722_0a95)
        });

        // The multiply mixes the high bits best, so they pick the map.
        &self.shards[((u128::from(hash) * count as u128) >> 64) as usize]
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

/// Runs `work` on the blocking pool inside a tokio runtime, so a walk of every entry or the freeing
/// of many values holds up no async thread, and here outside one.
fn off_async_thread(work: impl FnOnce() + Send + 'static) {
    match tokio::runtime::Handle::try_current() {
        Ok(runtime) => drop(runtime.spawn_blocking(work)),
        Err(_) => work(),
    }
}

/// The store's purge task: drops `store`'s expired entries every `interval`, on the blocking pool,
/// as a purge walks every entry. A panic there stops the task.
async fn purge_every(store: InMemoryCacheStore, interval: Duration) {
    loop {
        tokio::time::sleep(interval).await;

        let store = store.clone();

        if let Err(error) = tokio::task::spawn_blocking(move || store.purge()).await
            && error.is_panic()
        {
            std::panic::resume_unwind(error.into_panic());
        }
    }
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
/// drops. Inside a runtime, its purges, the trims of a full store and the freeing of what `clear`
/// took run on the blocking pool, so they hold up no async thread.
#[derive(Clone)]
pub struct InMemoryCacheStore {
    name: KeyPrefix,
    clock: Arc<dyn Clock>,
    settings: InMemoryCacheSettings,
    entries: Arc<Entries>,
    locker: Arc<Locker>,
    worker: Worker,
    reads: &'static Reads,
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
        let name = KeyPrefix::new(name)?;
        let settings = InMemoryCacheSettings::default();

        Ok(Self {
            reads: METRICS.reads(&name, Layer::Memory),
            name,
            clock: Arc::new(SystemClock),
            entries: Arc::new(Entries::new(settings.shards())),
            settings,
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

    /// Replaces the store's settings. Fails when one is out of range, such as a TTL over 365
    /// days, so a bad value stops setup instead of a later write. The store starts empty, split
    /// over `shards` maps.
    pub fn with_settings(mut self, settings: InMemoryCacheSettings) -> Result<Self, CacheError> {
        settings.validate()?;
        self.entries = Arc::new(Entries::new(settings.shards()));
        // New entries need a purge of their own; the store this came from keeps its own.
        self.worker = Worker::new();
        self.settings = settings;

        Ok(self)
    }

    /// The settings the store runs with.
    pub fn settings(&self) -> &InMemoryCacheSettings {
        &self.settings
    }

    /// Drops the expired entries and returns how many. The store's own purge task calls it every
    /// `purge_interval_secs`; with that off, call it on a schedule of your own. It walks every
    /// entry on the calling thread.
    #[tracing::instrument(level = "debug", name = "memory purge", skip_all, fields(store = %self.name, removed))]
    pub fn purge(&self) -> usize {
        let now = self.clock.instant();
        let removed = self.extract(|_, stored| stored.is_expired(now)).len();

        tracing::Span::current().record("removed", removed);

        if removed > 0 {
            tracing::debug!(store = %self.name, removed, "expired entries removed");
        }

        removed
    }

    /// Starts a trim down to about nine tenths of `max` unless one is running. Inside a tokio
    /// runtime it runs on the blocking pool, and the write that starts it goes on, as do the
    /// writes after it.
    fn start_trim(&self, max: usize) {
        if self.entries.trimming.swap(true, Ordering::Acquire) {
            return;
        }

        let trimming = Trimming(self.for_worker());

        off_async_thread(move || {
            let removed = trimming.0.trim(max);

            // Let the next trim start, then free what this one dropped.
            drop(trimming);
            drop(removed);
        });
    }

    /// Takes out expired entries, then the least recently used, down to about nine tenths of `max`
    /// so the next inserts need no work, and returns them for the caller to free. The count is
    /// checked again, as it may have gone down since the trim was started.
    fn trim(&self, max: usize) -> Vec<(String, StoredEntry)> {
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
                let (below, &mut cutoff, _) = uses.select_nth_unstable(excess - 1);
                // Entries read since the same write tie, so several can sit at the cutoff: take
                // only as many of those as the trim needs.
                let mut ties = excess - below.iter().filter(|&&used| used < cutoff).count();
                let evicted = self.extract(|_, stored| {
                    let used = stored.last_used.load(Ordering::Relaxed);

                    if used == cutoff && ties > 0 {
                        ties -= 1;
                        return true;
                    }

                    used < cutoff
                });

                METRICS.evicted(&self.name, evicted.len());
                tracing::debug!(
                    store = %self.name,
                    evicted = evicted.len(),
                    "store full; dropped the least recently used entries"
                );
                removed.extend(evicted);
            }
        }

        removed
    }

    /// A copy that shares the entries but not the `Worker`, for the store's purge worker and its
    /// trims, which would otherwise keep the purge running after the store's last clone drops.
    pub(crate) fn for_worker(&self) -> Self {
        Self {
            worker: Worker::none(),
            ..self.clone()
        }
    }

    /// Takes out every entry `pick` chooses, one map at a time. The caller frees them.
    fn extract(
        &self,
        mut pick: impl FnMut(&str, &StoredEntry) -> bool,
    ) -> Vec<(String, StoredEntry)> {
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

/// A started trim. Dropping it lets the next one start, even if it never ran, as when its runtime
/// shut down first.
struct Trimming(InMemoryCacheStore);

impl Drop for Trimming {
    fn drop(&mut self) {
        self.0.entries.trimming.store(false, Ordering::Release);
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
        // Read first even when the entry has no deadline: measured, reading it after the lookup,
        // only when needed, slows hits on entries with a TTL, which most entries have.
        let now = self.clock.instant();
        // An expired entry stays until a purge, a trim or a write replaces it.
        let found = read(self.entries.shard(key))
            .get(key)
            .filter(|stored| !stored.is_expired(now))
            .map(|stored| {
                let used = self.entries.uses.load(Ordering::Relaxed);

                // Stored only when it moved, so readers of one entry don't keep taking its cache
                // line from each other.
                if stored.last_used.load(Ordering::Relaxed) != used {
                    stored.last_used.store(used, Ordering::Relaxed);
                }

                stored.entry.clone()
            });

        let Some(entry) = found else {
            self.reads.record(false);

            return Ok(None);
        };

        let value = entry.value.downcast_ref::<CV>().cloned();

        // A value of another type is a miss too: the caller gets none.
        self.reads.record(value.is_some());

        let value = value.ok_or(CacheError::type_mismatch(type_name::<CV>()))?;

        Ok(Some(entry.map(|_| value)))
    }

    #[tracing::instrument(level = "debug", name = "memory set", skip_all, fields(key = %self.name.format_key(key)))]
    async fn set_entry<CV: CacheValue>(
        &self,
        key: &str,
        entry: CacheEntry<CV>,
    ) -> Result<(), CacheError> {
        // Entries only come in here, so the purge starts with the first one.
        if let Some(interval) = self.settings.purge_interval() {
            self.worker
                .start("in-memory-cache-purge-worker", &self.name, || {
                    purge_every(self.for_worker(), interval)
                });
        }

        let max_ttl = self.settings.max_ttl();
        // An entry that never expires, in a store that caps nothing, needs no clock. Otherwise the
        // monotonic clock goes first: measured, the other order slows writes with a TTL.
        let now = (entry.expires_at().is_some() || max_ttl.is_some()).then(|| self.clock.instant());
        // The store holds an entry for its remaining lifetime, or `max_ttl_secs` if that is
        // shorter.
        let remaining = entry
            .expires_at()
            .and_then(|_| entry.remaining_ttl(self.clock.now()));
        let ttl = match (remaining, max_ttl) {
            (Some(remaining), Some(cap)) => Some(remaining.min(cap)),
            (remaining, cap) => remaining.or(cap),
        };

        let mut entries = write(self.entries.shard(key));
        let replaced = entries.insert(
            key.to_owned(),
            StoredEntry {
                entry: entry.map(|value| Arc::new(value) as Erased),
                deadline: ttl.zip(now).and_then(|(ttl, now)| now.checked_add(ttl)),
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

        if let Some(max) = self.settings.max_entries
            && self.entries.len.load(Ordering::Relaxed) > max
        {
            self.start_trim(max);
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

        // The store is empty now; freeing what it held can take a while.
        off_async_thread(move || drop(cleared));

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
        ptr,
        sync::{Mutex, atomic::AtomicUsize, mpsc},
        time::Duration,
    };

    use futures_util::{FutureExt, future::join_all};
    use serde::{Deserialize, Serialize};
    use sisa_caching_core::{CacheKey, CacheOptions, settings::MAX_TTL_SECS};
    use sisa_caching_testing::{Logs, ManualClock, SlowDrop, eventually};

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

    /// Waits until the store's trim, which runs on the blocking pool, is done.
    async fn trimmed(store: &InMemoryCacheStore) {
        eventually("the trim ends", || async {
            !store.entries.trimming.load(Ordering::Acquire)
        })
        .await;
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
            Err(CacheError::TypeMismatch {
                expected: "u64",
                ..
            })
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
        // One that never expires is capped too.
        store.set(&key(2), 2_u32, None).await.unwrap();

        let entry = store.get_entry::<u32>(&key(1)).await.unwrap().unwrap();

        assert_eq!(entry.ttl(), Some(secs(60)));
        assert_eq!(held(&store, 1..3).await, [1, 2]);

        clock.advance(secs(10));
        assert!(held(&store, 1..3).await.is_empty());
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
    async fn the_purge_worker_purges_on_the_blocking_pool() {
        let clock = ManualClock::new();
        let store = store(&clock, InMemoryCacheSettings::default());
        let (slow, dropped, release) = SlowDrop::new();

        store.set(&key(1), slow, ttl(1)).await.unwrap();
        clock.advance(secs(1));
        // The worker's first purge, a minute after the write, frees the expired value.
        dropped.await.unwrap();

        release.send(()).expect("the purge held up the runtime");
    }

    #[tokio::test]
    async fn clear_empties_the_store_at_once_and_frees_the_values_on_the_blocking_pool() {
        let store = store(&ManualClock::new(), InMemoryCacheSettings::default());
        let (slow, dropped, release) = SlowDrop::new();

        store.set(&key(1), slow, None).await.unwrap();
        store.clear().await.unwrap();
        dropped.await.unwrap();

        assert!(held(&store, 1..2).await.is_empty());
        release.send(()).expect("the clear held up the runtime");
    }

    #[tokio::test(start_paused = true)]
    async fn the_longest_purge_interval_waits_without_panicking() {
        let store = store(&ManualClock::new(), InMemoryCacheSettings::default()).for_worker();
        let purging = purge_every(store, Duration::from_secs(MAX_TTL_SECS));

        assert!(
            tokio::time::timeout(secs(3600), purging).await.is_err(),
            "the task keeps waiting"
        );
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

    #[tokio::test]
    async fn a_full_store_drops_the_least_recently_used() {
        let store = store(&ManualClock::new(), with_limit(10));

        for id in 0..10_u32 {
            store.set(&key(id), id, None).await.unwrap();
        }
        // Reading 0 makes 1 and 2 the least recently used.
        store.get::<u32>(&key(0)).await.unwrap();
        store.set(&key(10), 10_u32, None).await.unwrap();
        trimmed(&store).await;

        assert_eq!(held(&store, 0..11).await, [0, 3, 4, 5, 6, 7, 8, 9, 10]);
    }

    #[tokio::test]
    async fn a_trim_drops_only_as_many_entries_read_together_as_it_needs() {
        let store = store(&ManualClock::new(), with_limit(10));

        for id in 0..10_u32 {
            store.set(&key(id), id, None).await.unwrap();
        }
        // Read since the same write, all ten tie as the least recently used.
        held(&store, 0..10).await;
        store.set(&key(10), 10_u32, None).await.unwrap();
        trimmed(&store).await;

        let held = held(&store, 0..11).await;

        assert_eq!(held.len(), 9, "{held:?}");
        assert!(held.contains(&10), "{held:?}");
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
        trimmed(&store).await;

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
        trimmed(&store).await;

        assert_eq!(held(&store, 0..11).await, [2, 3, 4, 5, 6, 7, 8, 9, 10]);
    }

    #[tokio::test]
    async fn a_trim_runs_on_the_blocking_pool_while_the_write_goes_on() {
        let store = store(&ManualClock::new(), with_limit(1));
        let (slow, dropped, release) = SlowDrop::new();

        store.set(&key(0), slow, None).await.unwrap();
        // Past the limit: the trim drops 0, the least recently used.
        store.set(&key(1), 1_u32, None).await.unwrap();
        dropped.await.unwrap();

        // The test runtime has one thread, so 0 is being freed on another while this runs.
        release.send(()).expect("the trim held up the write");
        assert_eq!(held(&store, 0..2).await, [1]);
    }

    #[test]
    fn keys_spread_over_every_map() {
        let entries = Entries::new(16);
        let mut counts = [0; 16];

        for id in 0..1_600 {
            let shard = entries.shard(&key(id));
            let index = entries.shards.iter().position(|it| ptr::eq(it, shard));

            counts[index.unwrap()] += 1;
        }

        // 100 on average.
        assert!(
            counts.iter().all(|count| (75..=125).contains(count)),
            "{counts:?}"
        );
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
        trimmed(&store).await;
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

    #[tokio::test]
    async fn get_or_set_logs_a_run_of_failed_reads_once_and_never_the_key() {
        let logs = Logs::default();
        let _logging = tracing::subscriber::set_default(logs.clone());
        let store = store(&ManualClock::new(), InMemoryCacheSettings::default());
        let load = || async { Ok::<_, Infallible>("ada".to_owned()) };
        let email = |id| key(format!("ada{id}@example.com"));

        // Each key holds another type, so both reads of each call fail.
        for id in 0..3 {
            store.set(&email(id), 42_u64, None).await.unwrap();
            store.get_or_set(&email(id), load, None).await.unwrap();
        }
        // What the last call stored over its key reads.
        store.get_or_set(&email(2), load, None).await.unwrap();

        let logged = logs.lines();
        let lines: Vec<&String> = logged
            .iter()
            .filter(|line| line.contains("operation="))
            .collect();

        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].starts_with("WARN"), "{lines:?}");
        assert!(lines[0].contains(r#"operation="get""#), "{lines:?}");
        assert!(lines[0].contains("store=app"), "{lines:?}");
        assert!(lines[1].starts_with("INFO"), "{lines:?}");
        assert!(lines[1].contains("store calls work again"), "{lines:?}");
        assert!(!logged.join(" ").contains("@example.com"), "{logged:?}");
    }

    #[tokio::test]
    async fn the_purge_worker_logs_its_start_and_stops_with_the_last_clone() {
        let logs = Logs::default();
        let _logging = tracing::subscriber::set_default(logs.clone());
        let store = store(&ManualClock::new(), InMemoryCacheSettings::default());
        let clone = store.clone();
        let entries = Arc::downgrade(&store.entries);
        let logged = |message: &str| logs.lines().iter().any(|line| line.contains(message));

        store.set(&key(1), 1_u32, None).await.unwrap();
        eventually("the worker starts", || async { logged("worker started") }).await;
        drop(store);
        drop(clone);
        eventually("the worker stops and drops its copy", || async {
            logged("worker stopped") && entries.upgrade().is_none()
        })
        .await;

        for line in logs.lines() {
            assert!(line.contains("store=app"), "{line}");
            assert!(line.contains("in-memory-cache-purge-worker"), "{line}");
        }
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
    async fn a_failed_refresh_serves_the_cached_value_only_while_the_store_holds_it() {
        let clock = ManualClock::new();
        let store = store(
            &clock,
            InMemoryCacheSettings {
                common: CacheSettings {
                    ttl_secs: Some(10),
                    refresh_threshold: Some(0.5),
                    ..CacheSettings::default()
                },
                max_ttl_secs: Some(8),
                ..InMemoryCacheSettings::default()
            },
        );
        let user = key(1);
        let refresh = |advance: u64| {
            let clock = clock.clone();

            store.get_or_set(
                &user,
                move || async move {
                    clock.advance(secs(advance));
                    Err::<String, _>("down")
                },
                None,
            )
        };

        // Still held: the cached value.
        store.set(&user, "old".to_owned(), None).await.unwrap();
        clock.advance(secs(6));
        assert_eq!(refresh(0).await.as_deref(), Ok("old"));

        // The store's 8-second cap passes during the load, though the entry's TTL is 10 seconds.
        assert_eq!(refresh(3).await, Err("down"));
        assert_eq!(store.get::<String>(&user).await.unwrap(), None);

        // The entry's own TTL passes during the load.
        store.set(&user, "old".to_owned(), ttl(4)).await.unwrap();
        clock.advance(secs(3));
        assert_eq!(refresh(2).await, Err("down"));

        // A write removes it during the load.
        store.set(&user, "old".to_owned(), None).await.unwrap();
        clock.advance(secs(6));

        let loaded = store
            .get_or_set(
                &user,
                || async {
                    store.remove(&user).await.unwrap();
                    Err::<String, _>("down")
                },
                None,
            )
            .await;

        assert_eq!(loaded, Err("down"));
    }

    type Hook = Box<dyn FnOnce() + Send>;

    /// A manual clock that runs a hook once, the next time a store reads the wall-clock time.
    #[derive(Clone)]
    struct HookClock(ManualClock, Arc<Mutex<Option<Hook>>>);

    impl Clock for HookClock {
        fn now(&self) -> std::time::SystemTime {
            let hook = self.1.lock().unwrap().take();

            if let Some(hook) = hook {
                hook();
            }

            self.0.now()
        }

        fn instant(&self) -> Instant {
            self.0.instant()
        }
    }

    #[tokio::test]
    async fn get_or_set_skips_a_refresh_another_caller_made_since_its_read() {
        let clock = HookClock(ManualClock::new(), Arc::default());
        let store = InMemoryCacheStore::new("app")
            .unwrap()
            .with_clock(clock.clone())
            .with_settings(InMemoryCacheSettings {
                common: CacheSettings {
                    ttl_secs: Some(10),
                    refresh_threshold: Some(0.5),
                    ..CacheSettings::default()
                },
                ..InMemoryCacheSettings::default()
            })
            .unwrap();
        let loads = Arc::new(AtomicUsize::new(0));
        let load = {
            let loads = Arc::clone(&loads);

            move || {
                loads.fetch_add(1, Ordering::SeqCst);
                async { Ok::<_, Infallible>("new".to_owned()) }
            }
        };

        store.set(&key(1), "old".to_owned(), None).await.unwrap();
        clock.0.advance(secs(6));

        // `get_or_set` reads the time once it has read the entry, before it takes the lock:
        // another caller refreshes the key right then.
        let (other, refresh) = (store.clone(), load.clone());

        *clock.1.lock().unwrap() = Some(Box::new(move || {
            let refreshed = other.get_or_set(&key(1), refresh, None).now_or_never();

            assert_eq!(refreshed.unwrap().as_deref(), Ok("new"));
        }));

        let loaded = store.get_or_set(&key(1), load, None).await;

        assert_eq!(loaded.as_deref(), Ok("new"));
        assert_eq!(loads.load(Ordering::SeqCst), 1);
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
