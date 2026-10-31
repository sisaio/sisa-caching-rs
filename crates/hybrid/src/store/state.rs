//! State shared by the hybrid store and its backplane worker.

use std::{
    collections::BTreeSet,
    hash::{BuildHasher, RandomState},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
};

use sisa_caching_core::{
    cache::{CacheEntry, CacheStore, CacheValue, KeyPrefix},
    locker::Locker,
};
use sisa_caching_memory::store::InMemoryCacheStore;
use tokio::{runtime::Handle, sync::RwLock};
use tracing::Span;

use crate::backplane::{Invalidation, NodeId};

const GENERATION_SLOTS: usize = 64;
/// Prefixes being removed at once, past which another prefix clears the local layer instead.
const REMOVING_LIMIT: usize = 16;

/// The local layer, the `get_or_set` lock, a generation number per slot of keys, and the sequence
/// numbers of the store's invalidations. Every invalidation bumps the generations it covers before
/// it evicts, so a reader that saw an older generation knows its distributed value may be stale
/// and doesn't copy it into the local layer. Promotions and generation bumps take turns, so a bump
/// never lands between a promotion's check and its copy.
///
/// The memory store's writes and removals never fail, so their results are dropped here.
pub(crate) struct State {
    pub(crate) local_store: InMemoryCacheStore,
    pub(crate) node_id: NodeId,
    /// Lets one `get_or_set` per key load at a time, across the store's clones.
    pub(crate) locker: Locker,
    generations: [AtomicU64; GENERATION_SLOTS],
    hasher: RandomState,
    /// Promotions share it; an invalidation takes it alone to bump generations.
    turns: RwLock<()>,
    local_reads_enabled: AtomicBool,
    /// The prefixes whose copies the blocking pool is still removing.
    removing: Arc<Removing>,
    /// Numbers this node's invalidations for its heartbeats.
    pub(crate) sequence: Arc<Sequence>,
    /// Whether `shutdown` has sent the stop message. A heartbeat holds it while it publishes, so
    /// none goes out after the stop.
    pub(crate) stopped: tokio::sync::Mutex<bool>,
}

impl State {
    pub(crate) fn new(local_store: InMemoryCacheStore) -> Self {
        Self {
            // Off until a backplane worker runs for this store.
            local_reads_enabled: AtomicBool::new(false),
            local_store,
            node_id: NodeId::generate(),
            locker: Locker::new(),
            generations: std::array::from_fn(|_| AtomicU64::default()),
            hasher: RandomState::new(),
            turns: RwLock::new(()),
            removing: Arc::default(),
            sequence: Arc::default(),
            stopped: tokio::sync::Mutex::new(false),
        }
    }

    fn slot(&self, key: &str) -> &AtomicU64 {
        &self.generations[self.hasher.hash_one(key) as usize % GENERATION_SLOTS]
    }

    /// The key's generation, which a read takes before it goes to the distributed store and hands
    /// to `promote`.
    pub(crate) fn generation(&self, key: &str) -> u64 {
        self.slot(key).load(Ordering::SeqCst)
    }

    pub(crate) fn local_reads_enabled(&self) -> bool {
        self.local_reads_enabled.load(Ordering::SeqCst)
    }

    pub(crate) fn disable_local_reads(&self) {
        self.local_reads_enabled.store(false, Ordering::SeqCst);
    }

    /// The key's local copy, when local reads are on and no prefix being removed covers it.
    pub(crate) async fn read<CV: CacheValue>(&self, key: &str) -> Option<CacheEntry<CV>> {
        if !self.local_reads_enabled() || self.removing.covers(key) {
            return None;
        }

        // A local copy of another Rust type reads as a miss: shared encoded data may still fit
        // the requested type.
        self.local_store.get_entry(key).await.ok().flatten()
    }

    /// Copies a distributed hit into the local layer, unless the key was invalidated since `seen`.
    /// A copy it replaces, as when two reads of a key both promote it, is freed after its turn, so
    /// dropping that value holds up no invalidation.
    pub(crate) async fn promote<CV: CacheValue>(
        &self,
        key: &str,
        seen: u64,
        entry: CacheEntry<CV>,
    ) {
        let replaced = {
            let _turn = self.turns.read().await;

            (self.local_reads_enabled() && self.generation(key) == seen)
                .then(|| self.local_store.replace_entry(key, entry))
        };

        drop(replaced);
    }

    /// Bumps every affected generation once no promotion is copying, then evicts the local copies.
    /// A promotion that starts after the bump sees the new generation and copies nothing, so the
    /// eviction runs outside the turn: dropping the evicted values holds up no other promotion.
    async fn invalidate(&self, invalidation: &Invalidation) {
        self.bump(invalidation).await;

        match invalidation {
            Invalidation::Key(key) => _ = self.local_store.remove(key).await,
            Invalidation::Prefix(prefix) => {
                _ = self.local_store.remove_prefix(prefix.as_str()).await
            }
            Invalidation::Clear => _ = self.local_store.clear().await,
            Invalidation::Heartbeat | Invalidation::Stopping => {}
        }
    }

    /// Applies an invalidation, this node's own or another's. A prefix's copies are removed on the
    /// blocking pool, as that walks every local entry, so neither the write nor the worker waits
    /// for it. The bump keeps a stale copy from landing after it, its keys read as misses until
    /// the walk is done, and the walk removes the copies from before. With `REMOVING_LIMIT` walks
    /// already queued, the prefix clears the local layer instead, which takes no walk.
    pub(crate) async fn apply(&self, invalidation: &Invalidation) {
        let Invalidation::Prefix(prefix) = invalidation else {
            return self.invalidate(invalidation).await;
        };

        // Bumped before the prefix is listed, as only its walk takes it off the list: a call
        // dropped while it waits for its turn, such as on a caller's timeout, then lists nothing.
        self.bump(invalidation).await;

        if !self.removing.add(prefix) {
            return self.invalidate(&Invalidation::Clear).await;
        }

        let (local, prefix, span) = (self.local_store.clone(), prefix.clone(), Span::current());
        let (removing, runtime) = (Arc::clone(&self.removing), Handle::current());

        // The memory store never waits, so `block_on` returns when the walk does.
        tokio::task::spawn_blocking(move || {
            span.in_scope(|| _ = runtime.block_on(local.remove_prefix(prefix.as_str())));
            removing.done(&prefix);
        });
    }

    /// Bumps the generations `invalidation` covers, once no promotion is copying.
    async fn bump(&self, invalidation: &Invalidation) {
        let _turn = self.turns.write().await;

        match invalidation {
            Invalidation::Key(key) => {
                self.slot(key).fetch_add(1, Ordering::SeqCst);
            }
            Invalidation::Prefix(_) | Invalidation::Clear => self.bump_all(),
            Invalidation::Heartbeat | Invalidation::Stopping => {}
        }
    }

    fn bump_all(&self) {
        for generation in &self.generations {
            generation.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Clears copies made before a delivery gap. Reads stay off while clearing, and come back on
    /// only if the subscription is connected.
    #[tracing::instrument(name = "hybrid resync", skip_all, fields(store = %self.local_store.name(), connected = connected))]
    pub(crate) async fn resync(&self, connected: bool) {
        self.local_reads_enabled.store(false, Ordering::SeqCst);
        self.invalidate(&Invalidation::Clear).await;
        self.local_reads_enabled.store(connected, Ordering::SeqCst);
    }
}

/// The prefixes whose local copies are being removed. A walk that never finishes, as when the
/// runtime shuts down, leaves its keys reading as misses, which is safe.
#[derive(Default)]
struct Removing {
    /// How many prefixes there are, so a read checks the list only while one is being removed.
    count: AtomicUsize,
    prefixes: Mutex<Vec<KeyPrefix>>,
}

impl Removing {
    fn prefixes(&self) -> MutexGuard<'_, Vec<KeyPrefix>> {
        self.prefixes.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn covers(&self, key: &str) -> bool {
        self.count.load(Ordering::SeqCst) > 0
            && self.prefixes().iter().any(|prefix| prefix.matches(key))
    }

    /// Adds `prefix`, unless `REMOVING_LIMIT` prefixes are already being removed.
    fn add(&self, prefix: &KeyPrefix) -> bool {
        let mut prefixes = self.prefixes();

        if prefixes.len() >= REMOVING_LIMIT {
            return false;
        }

        prefixes.push(prefix.clone());
        self.count.fetch_add(1, Ordering::SeqCst);

        true
    }

    fn done(&self, prefix: &KeyPrefix) {
        let mut prefixes = self.prefixes();

        if let Some(index) = prefixes.iter().position(|removing| removing == prefix) {
            prefixes.swap_remove(index);
            self.count.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

/// Hands out this node's sequence numbers, one to each write as it starts, and tracks which are
/// still being written or published, so a heartbeat only vouches for numbers whose publish is
/// over. Those were sent before the heartbeat, so every subscriber that gets them gets them first.
#[derive(Default)]
pub(crate) struct Sequence(Mutex<Numbers>);

#[derive(Default)]
struct Numbers {
    last: u64,
    publishing: BTreeSet<u64>,
}

impl Sequence {
    /// The next number, counted as publishing until the returned guard drops.
    pub(crate) fn start(self: &Arc<Self>) -> Publishing {
        let mut numbers = self.0.lock().unwrap_or_else(|e| e.into_inner());

        numbers.last += 1;

        let seq = numbers.last;

        numbers.publishing.insert(seq);

        Publishing {
            sequence: Arc::clone(self),
            seq,
        }
    }

    /// Whether no publish is in progress.
    pub(crate) fn idle(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .publishing
            .is_empty()
    }

    /// The highest number up to which every publish is over.
    pub(crate) fn finished(&self) -> u64 {
        let numbers = self.0.lock().unwrap_or_else(|e| e.into_inner());

        numbers
            .publishing
            .first()
            .map_or(numbers.last, |first| first - 1)
    }

    /// The last number handed out.
    pub(crate) fn last(&self) -> u64 {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).last
    }
}

/// One number being written or published.
pub(crate) struct Publishing {
    sequence: Arc<Sequence>,
    pub(crate) seq: u64,
}

impl Drop for Publishing {
    fn drop(&mut self) {
        self.sequence
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .publishing
            .remove(&self.seq);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::FutureExt;
    use sisa_caching_core::{
        cache::KeyPrefix,
        clock::{Clock, SystemClock},
    };
    use sisa_caching_testing::SlowDrop;
    use std::{
        sync::{Arc, Mutex},
        time::Duration,
    };
    use tokio::sync::oneshot;

    async fn state() -> State {
        let state = State::new(InMemoryCacheStore::new("state").unwrap());
        state.resync(true).await;
        state
    }

    fn key() -> String {
        "user:1".to_owned()
    }

    fn entry(state: &State, value: &str) -> CacheEntry<String> {
        CacheEntry::new(value.to_owned(), state.local_store.clock().now(), None)
    }

    #[tokio::test]
    async fn a_promotion_copies_a_hit_into_the_local_layer() {
        let state = state().await;
        let seen = state.generation(&key());

        assert!(state.read::<String>(&key()).await.is_none());

        state.promote(&key(), seen, entry(&state, "a")).await;

        let local = state.read::<String>(&key()).await;

        assert_eq!(local.map(|entry| entry.value).as_deref(), Some("a"));
    }

    #[tokio::test]
    async fn a_promotion_after_an_invalidation_is_dropped() {
        let state = state().await;
        let seen = state.generation(&key());

        state.invalidate(&Invalidation::Key(key())).await;
        state.promote(&key(), seen, entry(&state, "old")).await;

        assert!(state.read::<String>(&key()).await.is_none());
    }

    #[tokio::test]
    async fn a_resync_while_disconnected_turns_local_reads_off() {
        let state = state().await;
        state
            .promote(&key(), state.generation(&key()), entry(&state, "a"))
            .await;
        state.resync(false).await;

        assert!(!state.local_reads_enabled());
        assert!(state.read::<String>(&key()).await.is_none());

        state
            .promote(&key(), state.generation(&key()), entry(&state, "b"))
            .await;
        state.resync(true).await;

        assert!(state.read::<String>(&key()).await.is_none());
    }

    async fn promote(state: &State, key: &str) {
        state
            .promote(key, state.generation(key), entry(state, key))
            .await;
    }

    async fn local(state: &State, key: &str) -> Option<String> {
        state.read::<String>(key).await.map(|entry| entry.value)
    }

    /// A runtime whose one blocking thread waits until the returned sender drops.
    fn busy_blocking_pool() -> (tokio::runtime::Runtime, std::sync::mpsc::Sender<()>) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let (release, wait) = std::sync::mpsc::channel::<()>();

        drop(runtime.spawn_blocking(move || wait.recv()));

        (runtime, release)
    }

    #[test]
    fn a_prefix_hides_its_copies_until_the_walk_removes_them() {
        let (runtime, release) = busy_blocking_pool();

        runtime.block_on(async {
            let state = state().await;
            let keys = ["user:1", "user:2:a", "users:1", "order:1"];

            for key in keys {
                promote(&state, key).await;
            }

            state
                .apply(&Invalidation::Prefix(KeyPrefix::new("user").unwrap()))
                .await;

            // The walk hasn't run, but the prefix's keys already read as misses.
            assert_eq!(local(&state, "user:1").await, None);
            assert_eq!(local(&state, "user:2:a").await, None);
            assert_eq!(local(&state, "users:1").await.as_deref(), Some("users:1"));
            assert_eq!(local(&state, "order:1").await.as_deref(), Some("order:1"));

            drop(release);

            while state.removing.count.load(Ordering::SeqCst) > 0 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }

            let removed = state.local_store.get::<String>("user:1").await.unwrap();

            assert_eq!(removed, None);
            assert_eq!(local(&state, "order:1").await.as_deref(), Some("order:1"));

            // Once removed, the prefix's keys are copied again.
            promote(&state, "user:1").await;

            assert_eq!(local(&state, "user:1").await.as_deref(), Some("user:1"));
        });
    }

    #[test]
    fn a_prefix_past_the_queued_walks_clears_the_local_layer() {
        let (runtime, release) = busy_blocking_pool();

        runtime.block_on(async {
            let state = state().await;

            promote(&state, "order:1").await;

            for n in 0..REMOVING_LIMIT {
                let prefix = KeyPrefix::new(format!("p{n}")).unwrap();

                state.apply(&Invalidation::Prefix(prefix)).await;
            }

            assert_eq!(local(&state, "order:1").await.as_deref(), Some("order:1"));

            state
                .apply(&Invalidation::Prefix(KeyPrefix::new("user").unwrap()))
                .await;

            assert_eq!(local(&state, "order:1").await, None);

            drop(release);
        });
    }

    #[tokio::test]
    async fn a_prefix_removal_dropped_while_it_waits_for_its_turn_hides_nothing() {
        let state = state().await;

        promote(&state, "user:1").await;

        // A promotion is copying, so the prefix's bump waits for its turn.
        let turn = state.turns.read().await;
        let prefix = Invalidation::Prefix(KeyPrefix::new("user").unwrap());

        assert!(state.apply(&prefix).now_or_never().is_none());
        drop(turn);

        assert_eq!(local(&state, "user:1").await.as_deref(), Some("user:1"));
    }

    #[test]
    fn a_number_counts_as_finished_once_its_publish_and_every_earlier_one_is_over() {
        let sequence = Arc::new(Sequence::default());

        assert_eq!(sequence.finished(), 0);

        let first = sequence.start();
        let second = sequence.start();

        assert_eq!((first.seq, second.seq), (1, 2));
        assert_eq!(sequence.finished(), 0);

        drop(second);
        assert_eq!(sequence.finished(), 0, "1 is still publishing");

        drop(first);
        assert_eq!(sequence.finished(), 2);
    }

    type Hook = Box<dyn FnOnce() + Send>;

    /// The system clock, which runs a hook once, the next time a store reads the wall-clock time.
    #[derive(Clone, Default)]
    struct HookClock(Arc<Mutex<Option<Hook>>>);

    impl Clock for HookClock {
        fn now(&self) -> std::time::SystemTime {
            let hook = self.0.lock().unwrap().take();

            if let Some(hook) = hook {
                hook();
            }

            SystemClock.now()
        }

        fn instant(&self) -> std::time::Instant {
            SystemClock.instant()
        }
    }

    #[tokio::test]
    async fn an_invalidation_waits_for_a_promotion_then_evicts_its_copy() {
        let clock = HookClock::default();
        let state = Arc::new(State::new(
            InMemoryCacheStore::new("state")
                .unwrap()
                .with_clock(clock.clone()),
        ));
        state.resync(true).await;

        let seen = state.generation(&key());
        // One that expires, as the memory store reads the time in `set_entry` only for those.
        let ttl = Some(Duration::from_secs(60));
        let old = CacheEntry::new("old".to_owned(), SystemClock.now(), ttl);
        let waiting = Arc::new(Mutex::new(None));
        let (invalidated, started) = (Arc::clone(&state), Arc::clone(&waiting));

        // The memory store reads the time inside `set_entry`: after the promotion checked the
        // generation, before its copy lands. An invalidation that starts there can't run yet.
        *clock.0.lock().unwrap() = Some(Box::new(move || {
            let mut invalidation = Box::pin(async move {
                invalidated.invalidate(&Invalidation::Key(key())).await;
            });

            assert!((&mut invalidation).now_or_never().is_none());
            *started.lock().unwrap() = Some(invalidation);
        }));
        state.promote(&key(), seen, old).await;

        let local = state.read::<String>(&key()).await;
        assert_eq!(local.map(|entry| entry.value).as_deref(), Some("old"));

        let invalidation = waiting.lock().unwrap().take().unwrap();
        invalidation.await;

        assert!(state.read::<String>(&key()).await.is_none());
    }

    /// Copies a `SlowDrop` into the local layer under `key()`, and returns the receiver that hears
    /// its drop start and the sender that lets it finish.
    async fn promote_slow_drop(
        state: &State,
    ) -> (oneshot::Receiver<()>, std::sync::mpsc::Sender<()>) {
        let (slow, dropped, release) = SlowDrop::new();
        let seen = state.generation(&key());

        state
            .promote(&key(), seen, CacheEntry::new(slow, SystemClock.now(), None))
            .await;

        (dropped, release)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_slow_drop_during_an_invalidation_holds_up_no_promotion() {
        let state = Arc::new(state().await);
        let (dropped, release) = promote_slow_drop(&state).await;

        let invalidating = Arc::clone(&state);
        let invalidation = tokio::spawn(async move {
            invalidating.invalidate(&Invalidation::Key(key())).await;
        });
        // Wait until the invalidation drops the slow value. Polling the key could leave this task
        // the last reference, and the drop would then run here and wait on this task.
        dropped.await.unwrap();

        let other = "user:2";
        let seen = state.generation(other);
        let promotion = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            state.promote(other, seen, entry(&state, "a")),
        )
        .await;

        release.send(()).unwrap();
        invalidation.await.unwrap();
        assert!(promotion.is_ok(), "the promotion waited for the drop");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_slow_drop_of_a_copy_a_promotion_replaces_holds_up_no_invalidation() {
        let state = Arc::new(state().await);
        let (dropped, release) = promote_slow_drop(&state).await;

        // A second read of the key promotes it again, which replaces the slow value.
        let promoting = Arc::clone(&state);
        let promotion = tokio::spawn(async move {
            let seen = promoting.generation(&key());

            promoting
                .promote(&key(), seen, entry(&promoting, "a"))
                .await;
        });
        dropped.await.unwrap();

        let invalidation = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            state.invalidate(&Invalidation::Key("user:2".to_owned())),
        )
        .await;

        release.send(()).unwrap();
        promotion.await.unwrap();
        assert!(invalidation.is_ok(), "the invalidation waited for the drop");
    }

    #[tokio::test]
    async fn another_nodes_prefix_is_removed_on_the_blocking_pool_while_the_worker_goes_on() {
        let state = state().await;
        let (dropped, release) = promote_slow_drop(&state).await;

        state
            .apply(&Invalidation::Prefix(KeyPrefix::new("user").unwrap()))
            .await;
        dropped.await.unwrap();

        // The test runtime has one thread, so the walk frees the copy on another while the next
        // invalidation applies here.
        state.apply(&Invalidation::Key("user:2".to_owned())).await;
        release.send(()).expect("the walk held up the worker");
    }
}
