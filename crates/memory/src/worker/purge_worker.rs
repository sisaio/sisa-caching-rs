use std::time::Duration;

use sisa_caching_core::cache::PurgeableStore;

/// Drops a store's expired entries every `purge_interval_secs` of its settings. With no interval,
/// it stops at once.
#[derive(Debug)]
pub(crate) struct PurgeWorker<C> {
    cache: C,
    interval: Option<Duration>,
}

impl<C: PurgeableStore + Clone + Send + 'static> PurgeWorker<C> {
    /// A worker for `cache`. It reads the store's purge interval once, here.
    pub(crate) fn new(cache: C) -> Self {
        let interval = cache.purge_interval();

        Self { cache, interval }
    }

    pub(crate) async fn run(self) {
        let Some(interval) = self.interval else {
            return;
        };

        loop {
            tokio::time::sleep(interval).await;

            let cache = self.cache.clone();

            // A purge walks every entry, so it runs on the blocking pool. A panic there still
            // stops the worker.
            if let Err(error) = tokio::task::spawn_blocking(move || cache.purge()).await
                && error.is_panic()
            {
                std::panic::resume_unwind(error.into_panic());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use sisa_caching_core::{CacheKey, CacheOptions, CacheStore, settings::MAX_TTL_SECS};
    use sisa_caching_testing::ManualClock;

    use super::*;
    use crate::{settings::InMemoryCacheSettings, store::InMemoryCacheStore};

    fn purging_every(secs: Option<u64>) -> InMemoryCacheStore {
        InMemoryCacheStore::new("app")
            .unwrap()
            .with_settings(InMemoryCacheSettings {
                purge_interval_secs: secs,
                ..InMemoryCacheSettings::default()
            })
            .unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn the_worker_purges_every_interval() {
        let clock = ManualClock::new();
        // A copy without the store's `Worker`, so the store doesn't start a purge of its own.
        let store = purging_every(Some(60))
            .with_clock(clock.clone())
            .for_worker();
        let one_second = CacheOptions::with_ttl(Duration::from_secs(1));

        store
            .set(&CacheKey::new("user", 1).unwrap(), 1_u32, one_second)
            .await
            .unwrap();
        clock.advance(Duration::from_secs(1));

        let running = tokio::spawn(PurgeWorker::new(store.clone()).run());

        tokio::time::sleep(Duration::from_secs(61)).await;

        assert_eq!(store.purge(), 0, "the worker already purged it");
        running.abort();
    }

    #[tokio::test]
    async fn without_an_interval_the_worker_stops_at_once() {
        let worker = PurgeWorker::new(purging_every(None));

        tokio::time::timeout(Duration::from_secs(1), worker.run())
            .await
            .expect("the worker returns");
    }

    #[tokio::test(start_paused = true)]
    async fn the_longest_interval_waits_without_panicking() {
        let worker = PurgeWorker::new(purging_every(Some(MAX_TTL_SECS)));
        let waited = tokio::time::timeout(Duration::from_secs(3600), worker.run()).await;

        assert!(waited.is_err(), "the worker keeps waiting");
    }
}
