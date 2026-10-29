//! State shared by the hybrid store and its backplane worker.

use std::{
    collections::BTreeSet,
    hash::{BuildHasher, RandomState},
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use sisa_caching_core::{
    cache::{CacheEntry, CacheStore, CacheValue},
    locker::Locker,
};
use sisa_caching_memory::store::InMemoryCacheStore;
use tokio::{runtime::Handle, sync::RwLock};
use tracing::Span;

use crate::backplane::{Invalidation, NodeId};

const GENERATION_SLOTS: usize = 64;

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
    /// Numbers this node's invalidations for its heartbeats.
    pub(crate) sequence: Sequence,
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
            sequence: Sequence::default(),
        }
    }

    fn generation(&self, key: &str) -> &AtomicU64 {
        &self.generations[self.hasher.hash_one(key) as usize % GENERATION_SLOTS]
    }

    pub(crate) fn local_reads_enabled(&self) -> bool {
        self.local_reads_enabled.load(Ordering::SeqCst)
    }

    pub(crate) fn disable_local_reads(&self) {
        self.local_reads_enabled.store(false, Ordering::SeqCst);
    }

    /// The key's generation, and its local copy when local reads are on.
    pub(crate) async fn read<CV: CacheValue>(&self, key: &str) -> (u64, Option<CacheEntry<CV>>) {
        let generation = self.generation(key).load(Ordering::SeqCst);

        if !self.local_reads_enabled() {
            return (generation, None);
        }

        // A local copy of another Rust type reads as a miss: shared encoded data may still fit
        // the requested type.
        let local = self.local_store.get_entry(key).await.ok().flatten();

        (generation, local)
    }

    /// Copies a distributed hit into the local layer, unless the key was invalidated since `seen`.
    pub(crate) async fn promote<CV: CacheValue>(
        &self,
        key: &str,
        seen: u64,
        entry: CacheEntry<CV>,
    ) {
        let _turn = self.turns.read().await;

        if self.local_reads_enabled() && self.generation(key).load(Ordering::SeqCst) == seen {
            _ = self.local_store.set_entry(key, entry).await;
        }
    }

    /// Bumps every affected generation once no promotion is copying, then evicts the local copies.
    /// A promotion that starts after the bump sees the new generation and copies nothing, so the
    /// eviction runs outside the turn: dropping the evicted values holds up no other promotion.
    pub(crate) async fn invalidate(&self, invalidation: &Invalidation) {
        self.bump(invalidation).await;

        match invalidation {
            Invalidation::Key(key) => _ = self.local_store.remove(key).await,
            Invalidation::Prefix(prefix) => {
                _ = self.local_store.remove_prefix(prefix.as_str()).await
            }
            Invalidation::Clear => _ = self.local_store.clear().await,
            Invalidation::Heartbeat => {}
        }
    }

    /// Applies another node's invalidation. A prefix's copies are removed on the blocking pool, as
    /// that walks every local entry, so the worker goes on receiving meanwhile. The bump comes
    /// first, so no stale copy lands after it, and the walk removes the ones from before.
    pub(crate) async fn apply(&self, invalidation: &Invalidation) {
        let Invalidation::Prefix(prefix) = invalidation else {
            return self.invalidate(invalidation).await;
        };
        let (local, prefix, span) = (self.local_store.clone(), prefix.clone(), Span::current());
        let runtime = Handle::current();

        self.bump(invalidation).await;
        // The memory store never waits, so `block_on` returns when the walk does.
        tokio::task::spawn_blocking(move || {
            span.in_scope(|| _ = runtime.block_on(local.remove_prefix(prefix.as_str())))
        });
    }

    /// Bumps the generations `invalidation` covers, once no promotion is copying.
    async fn bump(&self, invalidation: &Invalidation) {
        let _turn = self.turns.write().await;

        match invalidation {
            Invalidation::Key(key) => {
                self.generation(key).fetch_add(1, Ordering::SeqCst);
            }
            Invalidation::Prefix(_) | Invalidation::Clear => self.bump_all(),
            Invalidation::Heartbeat => {}
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

/// Hands out this node's sequence numbers and tracks which are still being published, so a
/// heartbeat only vouches for numbers whose publish is over. Those were sent before the heartbeat,
/// so every subscriber that gets them gets them before it.
#[derive(Default)]
pub(crate) struct Sequence(Mutex<Numbers>);

#[derive(Default)]
struct Numbers {
    last: u64,
    publishing: BTreeSet<u64>,
}

impl Sequence {
    /// The next number, counted as publishing until the returned guard drops.
    pub(crate) fn start(&self) -> Publishing<'_> {
        let mut numbers = self.0.lock().unwrap_or_else(|e| e.into_inner());

        numbers.last += 1;

        let seq = numbers.last;

        numbers.publishing.insert(seq);

        Publishing {
            sequence: self,
            seq,
        }
    }

    /// The highest number up to which every publish is over.
    pub(crate) fn finished(&self) -> u64 {
        let numbers = self.0.lock().unwrap_or_else(|e| e.into_inner());

        numbers
            .publishing
            .first()
            .map_or(numbers.last, |first| first - 1)
    }
}

/// One number being published.
pub(crate) struct Publishing<'a> {
    sequence: &'a Sequence,
    pub(crate) seq: u64,
}

impl Drop for Publishing<'_> {
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
    use std::sync::{Arc, Mutex};
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
        let (seen, local) = state.read::<String>(&key()).await;

        assert!(local.is_none());

        state.promote(&key(), seen, entry(&state, "a")).await;

        let (_, local) = state.read::<String>(&key()).await;

        assert_eq!(local.map(|entry| entry.value).as_deref(), Some("a"));
    }

    #[tokio::test]
    async fn a_promotion_after_an_invalidation_is_dropped() {
        let state = state().await;
        let (seen, _) = state.read::<String>(&key()).await;

        state.invalidate(&Invalidation::Key(key())).await;
        state.promote(&key(), seen, entry(&state, "old")).await;

        assert!(state.read::<String>(&key()).await.1.is_none());
    }

    #[tokio::test]
    async fn a_resync_while_disconnected_turns_local_reads_off() {
        let state = state().await;
        let (seen, _) = state.read::<String>(&key()).await;

        state.promote(&key(), seen, entry(&state, "a")).await;
        state.resync(false).await;

        assert!(!state.local_reads_enabled());
        assert!(state.read::<String>(&key()).await.1.is_none());

        let (seen, _) = state.read::<String>(&key()).await;

        state.promote(&key(), seen, entry(&state, "b")).await;
        state.resync(true).await;

        assert!(state.read::<String>(&key()).await.1.is_none());
    }

    #[test]
    fn a_number_counts_as_finished_once_its_publish_and_every_earlier_one_is_over() {
        let sequence = Sequence::default();

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

        let (seen, _) = state.read::<String>(&key()).await;
        let old = entry(&state, "old");
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

        let (_, local) = state.read::<String>(&key()).await;
        assert_eq!(local.map(|entry| entry.value).as_deref(), Some("old"));

        let invalidation = waiting.lock().unwrap().take().unwrap();
        invalidation.await;

        assert!(state.read::<String>(&key()).await.1.is_none());
    }

    /// A value whose drop says on its oneshot that it started, then waits up to five seconds until
    /// the test sends on its gate, or drops the gate's sender. A drop that runs on the test's own
    /// thread holds the test up that long, and the test's send then fails. Its clones drop at once.
    #[derive(serde::Serialize, serde::Deserialize)]
    struct SlowDrop {
        #[serde(skip)]
        gate: Option<(oneshot::Sender<()>, Mutex<std::sync::mpsc::Receiver<()>>)>,
    }

    impl Clone for SlowDrop {
        fn clone(&self) -> Self {
            Self { gate: None }
        }
    }

    impl Drop for SlowDrop {
        fn drop(&mut self) {
            if let Some((dropping, gate)) = self.gate.take() {
                _ = dropping.send(());
                _ = gate
                    .lock()
                    .unwrap()
                    .recv_timeout(std::time::Duration::from_secs(5));
            }
        }
    }

    /// Copies a `SlowDrop` into the local layer under `key()`, and returns the receiver that hears
    /// its drop start and the sender that lets it finish.
    async fn promote_slow_drop(
        state: &State,
    ) -> (oneshot::Receiver<()>, std::sync::mpsc::Sender<()>) {
        let (release, gate) = std::sync::mpsc::channel();
        let (dropping, dropped) = oneshot::channel();
        let (seen, _) = state.read::<SlowDrop>(&key()).await;
        let slow = SlowDrop {
            gate: Some((dropping, Mutex::new(gate))),
        };

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
        let (seen, _) = state.read::<String>(other).await;
        let promotion = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            state.promote(other, seen, entry(&state, "a")),
        )
        .await;

        release.send(()).unwrap();
        invalidation.await.unwrap();
        assert!(promotion.is_ok(), "the promotion waited for the drop");
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

    #[tokio::test]
    async fn a_resync_empties_the_local_layer_at_once_and_frees_it_on_the_blocking_pool() {
        let state = state().await;
        let (dropped, release) = promote_slow_drop(&state).await;

        state.resync(true).await;
        dropped.await.unwrap();

        assert!(state.read::<SlowDrop>(&key()).await.1.is_none());
        release.send(()).expect("the resync held up the worker");
    }
}
