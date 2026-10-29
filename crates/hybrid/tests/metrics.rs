//! The metrics a hybrid store records, above all the two the docs say to alert on. A test binary of
//! its own: the instruments are created on the first cache call, from the meter provider installed
//! then, so this test must install its provider before any other test touches a store.

mod common;

use common::{key, memory, node, publish};
use opentelemetry::{KeyValue, global};
use opentelemetry_sdk::metrics::{
    InMemoryMetricExporter, PeriodicReader, SdkMeterProvider,
    data::{AggregatedMetrics, MetricData, ResourceMetrics},
};
use sisa_caching_core::cache::CacheStore;
use sisa_caching_testing::{Redis, eventually};

/// The latest value of the counter or gauge `name`, or the count of the histogram `name`, with
/// attributes `store=app` plus `attributes`; 0 when it was never recorded.
fn value(exported: &[ResourceMetrics], name: &str, attributes: &[KeyValue]) -> u64 {
    let wanted = [&[KeyValue::new("store", "app")], attributes].concat();
    let matches = |found: Vec<&KeyValue>| {
        found.len() == wanted.len() && wanted.iter().all(|kv| found.contains(&kv))
    };

    exported
        .iter()
        .flat_map(|resource| resource.scope_metrics())
        .flat_map(|scope| scope.metrics())
        .filter(|metric| metric.name() == name)
        .flat_map(|metric| -> Vec<u64> {
            match metric.data() {
                AggregatedMetrics::U64(MetricData::Sum(sum)) => sum
                    .data_points()
                    .filter(|point| matches(point.attributes().collect()))
                    .map(|point| point.value())
                    .collect(),
                AggregatedMetrics::U64(MetricData::Gauge(gauge)) => gauge
                    .data_points()
                    .filter(|point| matches(point.attributes().collect()))
                    .map(|point| point.value())
                    .collect(),
                AggregatedMetrics::F64(MetricData::Histogram(histogram)) => histogram
                    .data_points()
                    .filter(|point| matches(point.attributes().collect()))
                    .map(|point| point.count())
                    .collect(),
                _ => Vec::new(),
            }
        })
        .last()
        .unwrap_or_default()
}

#[tokio::test]
async fn a_hybrid_store_counts_reads_loads_invalidations_and_its_backplane() {
    let exporter = InMemoryMetricExporter::default();
    let provider = SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(exporter.clone()).build())
        .build();

    global::set_meter_provider(provider.clone());

    let metric = |name: &str, attributes: &[KeyValue]| {
        provider.force_flush().unwrap();

        value(&exporter.get_finished_metrics().unwrap(), name, attributes)
    };
    let hybrid = [KeyValue::new("layer", "hybrid")];
    let redis = Redis::start().await;
    let distributed = memory("app");
    let writer = node(&distributed, &redis).await;
    let reader = node(&distributed, &redis).await;

    // Local reads come on just before the gauge is set, so wait for it.
    eventually("the workers report the backplane up", || async {
        metric("sisa.caching.backplane.connected", &[]) == 1
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
        metric("sisa.caching.invalidations.received", &[]) == 2
    })
    .await;
    // Each reached both subscriptions, the writer's own included.
    eventually("both publishes count their subscribers", || async {
        metric("sisa.caching.invalidations.delivered", &[]) == 4
    })
    .await;
    assert_eq!(metric("sisa.caching.hits", &hybrid), 2);
    assert_eq!(metric("sisa.caching.misses", &hybrid), 2);
    assert_eq!(
        metric(
            "sisa.caching.load.duration",
            &[KeyValue::new("outcome", "ok")]
        ),
        1
    );

    // A message no node can read counts once on each.
    publish(&redis, "not json").await;
    eventually("both nodes count it", || async {
        metric("sisa.caching.backplane.unreadable", &[]) == 2
    })
    .await;

    // With the server gone, every try fails, which counts as one failed publish.
    redis.stop().await;
    writer.remove(&key(1)).await.unwrap();

    eventually("the publish gives up", || async {
        metric("sisa.caching.publish.failures", &[]) == 1
    })
    .await;
    eventually("the workers report the backplane down", || async {
        metric("sisa.caching.backplane.connected", &[]) == 0
    })
    .await;
}
