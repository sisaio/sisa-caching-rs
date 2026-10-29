//! Writes that fail, panic, or whose caller gives up half way: the other nodes must still hear of
//! them, since the distributed store may have applied them anyway.

mod common;

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use common::{WRITES, Write, backplane, key, local_value, memory, node, start};
use sisa_caching_core::{
    cache::{CacheEntry, CacheError, CacheStore, CacheValue, KeyPrefix},
    locker::Locker,
    settings::CacheSettings,
};
use sisa_caching_hybrid::{error::HybridCacheError, store::HybridCacheStore};
use sisa_caching_memory::store::InMemoryCacheStore;
use sisa_caching_testing::{Redis, eventually};
use tokio::sync::Semaphore;

/// Applies every write, then reports a failure, as a call that timed out after Redis applied it
/// would.
struct LostReplies(InMemoryCacheStore);

#[derive(Debug, thiserror::Error)]
enum LostReply {
    #[error("the reply was lost")]
    Lost,

    #[error(transparent)]
    Cache(#[from] CacheError),
}

impl CacheStore for LostReplies {
    type Error = LostReply;

    fn name(&self) -> &KeyPrefix {
        self.0.name()
    }

    fn cache_settings(&self) -> &CacheSettings {
        self.0.cache_settings()
    }

    fn locker(&self) -> &Locker {
        self.0.locker()
    }

    async fn get_entry<CV: CacheValue>(
        &self,
        key: &str,
    ) -> Result<Option<CacheEntry<CV>>, LostReply> {
        Ok(self.0.get_entry(key).await?)
    }

    async fn set_entry<CV: CacheValue>(
        &self,
        key: &str,
        entry: CacheEntry<CV>,
    ) -> Result<(), LostReply> {
        self.0.set_entry(key, entry).await?;

        Err(LostReply::Lost)
    }

    async fn remove(&self, key: &str) -> Result<bool, LostReply> {
        self.0.remove(key).await?;

        Err(LostReply::Lost)
    }

    async fn remove_prefix(&self, prefix: &str) -> Result<usize, LostReply> {
        self.0.remove_prefix(prefix).await?;

        Err(LostReply::Lost)
    }

    async fn clear(&self) -> Result<(), LostReply> {
        self.0.clear().await?;

        Err(LostReply::Lost)
    }
}

#[tokio::test]
async fn a_failed_write_still_evicts_on_every_node() {
    let redis = Redis::start().await;

    for write in WRITES {
        let shared = memory("users");
        let writer = start(
            HybridCacheStore::new(
                memory("users"),
                LostReplies(shared.clone()),
                backplane(&redis).await,
            )
            .unwrap(),
        )
        .await;
        let reader = node(&shared, &redis).await;

        shared.set(&key(1), "old".to_owned(), None).await.unwrap();
        writer.get::<String>(&key(1)).await.unwrap();
        reader.get::<String>(&key(1)).await.unwrap();

        assert!(
            write.run(&writer).await.is_err(),
            "{write:?}: the caller sees the failure"
        );
        assert_eq!(local_value(&writer, &key(1)).await, None, "{write:?}");
        eventually(&format!("{write:?}: the reader drops its copy"), || async {
            local_value(&reader, &key(1)).await.is_none()
        })
        .await;
    }
}

/// Points where a [`GatedStore`] write stops until the test lets it go on.
struct Gate {
    /// Stop before writing.
    before: AtomicBool,
    /// Stop after writing.
    after: AtomicBool,
    /// Panic after writing.
    panic: AtomicBool,
    started: Semaphore,
    written: Semaphore,
    release: Semaphore,
}

impl Default for Gate {
    fn default() -> Self {
        Self {
            before: AtomicBool::new(false),
            after: AtomicBool::new(false),
            panic: AtomicBool::new(false),
            started: Semaphore::new(0),
            written: Semaphore::new(0),
            release: Semaphore::new(0),
        }
    }
}

/// A memory store whose writes pass through a [`Gate`]. Not `Clone`: the hybrid store must keep
/// its distributed store alive itself.
struct GatedStore {
    inner: InMemoryCacheStore,
    gate: Arc<Gate>,
}

impl GatedStore {
    async fn write<T>(
        &self,
        write: impl Future<Output = Result<T, CacheError>>,
    ) -> Result<T, CacheError> {
        self.gate.started.add_permits(1);

        if self.gate.before.load(Ordering::SeqCst) {
            self.gate.release.acquire().await.unwrap().forget();
        }

        let result = write.await;

        self.gate.written.add_permits(1);
        assert!(
            !self.gate.panic.load(Ordering::SeqCst),
            "the store panicked after writing"
        );

        if self.gate.after.load(Ordering::SeqCst) {
            self.gate.release.acquire().await.unwrap().forget();
        }

        result
    }
}

impl CacheStore for GatedStore {
    type Error = CacheError;

    fn name(&self) -> &KeyPrefix {
        self.inner.name()
    }

    fn cache_settings(&self) -> &CacheSettings {
        self.inner.cache_settings()
    }

    fn locker(&self) -> &Locker {
        self.inner.locker()
    }

    async fn get_entry<CV: CacheValue>(
        &self,
        key: &str,
    ) -> Result<Option<CacheEntry<CV>>, CacheError> {
        self.inner.get_entry(key).await
    }

    async fn set_entry<CV: CacheValue>(
        &self,
        key: &str,
        entry: CacheEntry<CV>,
    ) -> Result<(), CacheError> {
        self.write(self.inner.set_entry(key, entry)).await
    }

    async fn remove(&self, key: &str) -> Result<bool, CacheError> {
        self.write(self.inner.remove(key)).await
    }

    async fn remove_prefix(&self, prefix: &str) -> Result<usize, CacheError> {
        self.write(self.inner.remove_prefix(prefix)).await
    }

    async fn clear(&self) -> Result<(), CacheError> {
        self.write(self.inner.clear()).await
    }
}

type GatedNode = HybridCacheStore<GatedStore>;
type Reader = HybridCacheStore<InMemoryCacheStore>;

/// A writer whose writes pass through `gate`, and a reader on the same stores. Both hold a copy
/// of `user:1` = `old`.
async fn writer_and_reader(gate: &Arc<Gate>, redis: &Redis) -> (GatedNode, Reader) {
    let shared = memory("users");

    shared.set(&key(1), "old".to_owned(), None).await.unwrap();

    let distributed = GatedStore {
        inner: shared.clone(),
        gate: gate.clone(),
    };
    let writer =
        start(HybridCacheStore::new(memory("users"), distributed, backplane(redis).await).unwrap())
            .await;
    let reader =
        start(HybridCacheStore::new(memory("users"), shared, backplane(redis).await).unwrap())
            .await;

    writer.get::<String>(&key(1)).await.unwrap();
    reader.get::<String>(&key(1)).await.unwrap();

    (writer, reader)
}

/// Waits for one permit, so the test knows the write reached that point.
async fn reached(point: &Semaphore) {
    tokio::time::timeout(Duration::from_secs(2), point.acquire())
        .await
        .expect("the write reaches the point")
        .unwrap()
        .forget();
}

async fn reader_sees(reader: &Reader, expected: Option<&str>) {
    eventually("the reader drops its copy", || async {
        local_value(reader, &key(1)).await.is_none()
    })
    .await;
    assert_eq!(
        reader.get::<String>(&key(1)).await.unwrap().as_deref(),
        expected
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_write_whose_caller_gives_up_after_it_was_applied_still_reaches_other_nodes() {
    let redis = Redis::start().await;

    for write in WRITES {
        let gate = Arc::new(Gate::default());
        let (writer, reader) = writer_and_reader(&gate, &redis).await;

        gate.after.store(true, Ordering::SeqCst);

        let call = tokio::spawn({
            let writer = writer.clone();
            async move { write.run(&writer).await }
        });

        reached(&gate.written).await;
        call.abort();
        assert!(call.await.unwrap_err().is_cancelled());
        assert_eq!(
            writer.get::<String>(&key(1)).await.unwrap().as_deref(),
            write.result(),
            "{write:?}"
        );

        gate.release.add_permits(1);
        reader_sees(&reader, write.result()).await;

        // The writer copies distributed hits into its local layer again afterwards.
        writer.local_store().clear().await.unwrap();
        writer
            .distributed_store()
            .inner
            .set(&key(1), "later".to_owned(), None)
            .await
            .unwrap();
        writer.get::<String>(&key(1)).await.unwrap();
        assert_eq!(
            local_value(&writer, &key(1)).await.as_deref(),
            Some("later"),
            "{write:?}"
        );
    }
}

#[tokio::test]
async fn a_write_whose_caller_gives_up_before_it_was_applied_still_runs() {
    let redis = Redis::start().await;
    let gate = Arc::new(Gate::default());
    let (writer, _) = writer_and_reader(&gate, &redis).await;

    gate.before.store(true, Ordering::SeqCst);

    let call = tokio::spawn({
        let writer = writer.clone();
        async move { Write::Set.run(&writer).await }
    });

    reached(&gate.started).await;
    call.abort();
    assert!(call.await.unwrap_err().is_cancelled());
    assert_eq!(
        writer.get::<String>(&key(1)).await.unwrap().as_deref(),
        Some("old")
    );

    gate.release.add_permits(1);
    reached(&gate.written).await;
    assert_eq!(
        writer.get::<String>(&key(1)).await.unwrap().as_deref(),
        Some("new")
    );
}

#[tokio::test]
async fn a_write_runs_to_the_end_after_its_last_handle_is_dropped() {
    let redis = Redis::start().await;
    let gate = Arc::new(Gate::default());
    let (writer, reader) = writer_and_reader(&gate, &redis).await;

    gate.after.store(true, Ordering::SeqCst);

    // The task owns the writer's last handle.
    let call = tokio::spawn(async move { Write::Set.run(&writer).await });

    reached(&gate.written).await;
    call.abort();
    assert!(call.await.unwrap_err().is_cancelled());

    gate.release.add_permits(1);
    reader_sees(&reader, Some("new")).await;
}

#[tokio::test]
async fn a_write_returns_without_waiting_for_its_publish_which_still_goes_out() {
    let redis = Redis::start().await;
    let gate = Arc::new(Gate::default());
    let (writer, reader) = writer_and_reader(&gate, &redis).await;

    // Frozen, the server holds the publish. A write that waited for it would take at least the
    // 500 ms reply timeout.
    redis.pause().await;
    tokio::time::timeout(Duration::from_millis(400), Write::Set.run(&writer))
        .await
        .expect("the write returns while its publish is held")
        .unwrap();
    assert_eq!(local_value(&reader, &key(1)).await.as_deref(), Some("old"));

    redis.unpause().await;
    reader_sees(&reader, Some("new")).await;
}

#[tokio::test]
async fn a_write_that_panics_returns_an_error_and_still_reaches_other_nodes() {
    let redis = Redis::start().await;
    let gate = Arc::new(Gate::default());
    let (writer, reader) = writer_and_reader(&gate, &redis).await;

    gate.panic.store(true, Ordering::SeqCst);

    assert!(matches!(
        Write::Set.run(&writer).await,
        Err(HybridCacheError::WriteStopped(_))
    ));
    // The value was written before the panic.
    reader_sees(&reader, Some("new")).await;
    assert_eq!(
        writer.get::<String>(&key(1)).await.unwrap().as_deref(),
        Some("new")
    );
}
