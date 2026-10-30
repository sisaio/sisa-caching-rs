//! The metrics a hybrid store records, above all the two the docs say to alert on. A test binary of
//! its own: the instruments are created when the first store is built, from the meter provider
//! installed then, so this test must install its provider before any other test builds a store.

mod common;

use common::{key, memory, node, publish};
use opentelemetry::KeyValue;
use sisa_caching_core::cache::CacheStore;
use sisa_caching_testing::{Metrics, Redis, eventually};

#[tokio::test]
async fn a_hybrid_store_counts_reads_loads_invalidations_and_its_backplane() {
    let metrics = Metrics::install();
    let hybrid = [KeyValue::new("layer", "hybrid")];
    let redis = Redis::start().await;
    let distributed = memory("app");
    let writer = node(&distributed, &redis).await;
    let reader = node(&distributed, &redis).await;

    // Local reads come on just before the gauge is set, so wait for it.
    eventually("the workers report the backplane up", || async {
        metrics.value("sisa.caching.backplane.connected", &[]) == 1
    })
    .await;

    // Two hits for the hybrid as a whole, whichever layer serves the second.
    writer.set(&key(1), "ada".to_owned(), None).await.unwrap();
    reader.get::<String>(&key(1)).await.unwrap();
    reader.get::<String>(&key(1)).await.unwrap();
    // A load: two misses, one before and one after the lock, and one duration.
    writer
        .get_or_set(&key(2), || async { Ok::<_, ()>("grace".to_owned()) }, None)
        .await
        .unwrap();

    eventually("the reader applies both writes", || async {
        metrics.value("sisa.caching.invalidations.received", &[]) == 2
    })
    .await;
    // Each reached both subscriptions, the writer's own included.
    eventually("both publishes count their subscribers", || async {
        metrics.value("sisa.caching.invalidations.delivered", &[]) == 4
    })
    .await;
    assert_eq!(metrics.value("sisa.caching.hits", &hybrid), 2);
    assert_eq!(metrics.value("sisa.caching.misses", &hybrid), 2);
    assert_eq!(
        metrics.value(
            "sisa.caching.load.duration",
            &[KeyValue::new("outcome", "ok")]
        ),
        1
    );

    // A message no node can read counts once on each.
    publish(&redis, "app", "not json").await;
    eventually("both nodes count it", || async {
        metrics.value("sisa.caching.backplane.unreadable", &[]) == 2
    })
    .await;
    assert_eq!(
        metrics.value(
            "sisa.caching.local.clears",
            &[KeyValue::new("reason", "unreadable")]
        ),
        2
    );

    // With the server gone, every try fails, which counts as one failed publish.
    redis.stop().await;
    writer.remove(&key(1)).await.unwrap();

    eventually("the publish gives up", || async {
        metrics.value("sisa.caching.publish.failures", &[]) == 1
    })
    .await;
    eventually("the workers report the backplane down", || async {
        metrics.value("sisa.caching.backplane.connected", &[]) == 0
    })
    .await;
}
