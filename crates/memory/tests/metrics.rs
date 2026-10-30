//! The hit, miss and eviction counters a memory store records. A test binary of its own: the
//! instruments are created when the first store is built, from the meter provider installed then,
//! so this test must install its provider before any other test builds a store.

use opentelemetry::KeyValue;
use sisa_caching_core::cache::{CacheKey, CacheStore};
use sisa_caching_memory::{settings::InMemoryCacheSettings, store::InMemoryCacheStore};
use sisa_caching_testing::{Metrics, eventually};

#[tokio::test]
async fn a_store_counts_hits_misses_and_evictions() {
    let metrics = Metrics::install();

    let store = InMemoryCacheStore::new("app")
        .unwrap()
        .with_settings({
            let mut settings = InMemoryCacheSettings::default();
            settings.max_entries = Some(10);
            settings
        })
        .unwrap();

    for id in 0..11 {
        store
            .set(&CacheKey::new("user", id).unwrap(), id, None)
            .await
            .unwrap();
    }

    // The 11th entry started a trim down to 9, which runs on the blocking pool: the two least
    // recently used, 0 and 1, go.
    eventually("the trim drops two entries", || async {
        metrics.value("sisa.caching.evictions", &[]) == 2
    })
    .await;
    assert_eq!(
        store
            .get::<i32>(&CacheKey::new("user", 10).unwrap())
            .await
            .unwrap(),
        Some(10)
    );
    assert_eq!(
        store
            .get::<i32>(&CacheKey::new("user", 0).unwrap())
            .await
            .unwrap(),
        None
    );
    // A value of another type is a miss too: the caller gets none.
    assert!(
        store
            .get::<String>(&CacheKey::new("user", 10).unwrap())
            .await
            .is_err()
    );

    let memory = [KeyValue::new("layer", "memory")];

    assert_eq!(metrics.value("sisa.caching.hits", &memory), 1);
    assert_eq!(metrics.value("sisa.caching.misses", &memory), 2);
    assert_eq!(metrics.value("sisa.caching.evictions", &[]), 2);
}
