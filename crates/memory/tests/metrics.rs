//! The hit, miss and eviction counters a memory store records. A test binary of its own: the
//! instruments are created when the first store is built, from the meter provider installed then,
//! so this test must install its provider before any other test builds a store.

use opentelemetry::{KeyValue, global};
use opentelemetry_sdk::metrics::{
    InMemoryMetricExporter, PeriodicReader, SdkMeterProvider,
    data::{AggregatedMetrics, MetricData, ResourceMetrics},
};
use sisa_caching_core::cache::{CacheKey, CacheStore};
use sisa_caching_memory::{settings::InMemoryCacheSettings, store::InMemoryCacheStore};
use sisa_caching_testing::eventually;

/// The counter `name` with attributes `store=app` plus `attributes`, or 0 when it was never
/// recorded.
fn counter(exported: &[ResourceMetrics], name: &str, attributes: &[KeyValue]) -> u64 {
    let wanted = [&[KeyValue::new("store", "app")], attributes].concat();

    exported
        .iter()
        .flat_map(|resource| resource.scope_metrics())
        .flat_map(|scope| scope.metrics())
        .filter(|metric| metric.name() == name)
        .filter_map(|metric| match metric.data() {
            AggregatedMetrics::U64(MetricData::Sum(sum)) => Some(sum),
            _ => None,
        })
        .flat_map(|sum| sum.data_points())
        .filter(|point| {
            let found: Vec<_> = point.attributes().collect();

            found.len() == wanted.len() && wanted.iter().all(|kv| found.contains(&kv))
        })
        .map(|point| point.value())
        .last()
        .unwrap_or_default()
}

#[tokio::test]
async fn a_store_counts_hits_misses_and_evictions() {
    let exporter = InMemoryMetricExporter::default();
    let provider = SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(exporter.clone()).build())
        .build();

    global::set_meter_provider(provider.clone());

    let store = InMemoryCacheStore::new("app")
        .unwrap()
        .with_settings(InMemoryCacheSettings {
            max_entries: Some(10),
            ..Default::default()
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
        provider.force_flush().unwrap();

        let exported = exporter.get_finished_metrics().unwrap();

        counter(&exported, "sisa.caching.evictions", &[]) == 2
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

    provider.force_flush().unwrap();

    let exported = exporter.get_finished_metrics().unwrap();
    let memory = [KeyValue::new("layer", "memory")];

    assert_eq!(counter(&exported, "sisa.caching.hits", &memory), 1);
    assert_eq!(counter(&exported, "sisa.caching.misses", &memory), 2);
    assert_eq!(counter(&exported, "sisa.caching.evictions", &[]), 2);
}
