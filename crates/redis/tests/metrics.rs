//! What a Redis store counts and logs while its server is down. A test binary of its own: the
//! instruments are created on the first cache call, from the meter provider installed then, so
//! this test must install its provider before any other test touches a store.

use opentelemetry::{KeyValue, global};
use opentelemetry_sdk::metrics::{
    InMemoryMetricExporter, PeriodicReader, SdkMeterProvider,
    data::{AggregatedMetrics, MetricData, ResourceMetrics},
};
use sisa_caching_core::cache::{CacheKey, CacheStore};
use sisa_caching_redis::store::RedisCacheStore;
use sisa_caching_testing::{Logs, Redis, eventually};

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

fn key(id: u64) -> CacheKey {
    CacheKey::new("user", id).unwrap()
}

#[tokio::test]
async fn an_outage_logs_once_per_operation_and_counts_every_failure() {
    let exporter = InMemoryMetricExporter::default();
    let provider = SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(exporter.clone()).build())
        .build();

    global::set_meter_provider(provider.clone());

    let metric = |name: &str, attributes: &[KeyValue]| {
        provider.force_flush().unwrap();

        counter(&exporter.get_finished_metrics().unwrap(), name, attributes)
    };
    let logs = Logs::default();
    let _logging = tracing::subscriber::set_default(logs.clone());
    let redis = Redis::start_restartable().await;
    let store = RedisCacheStore::connect("app", redis.url()).await.unwrap();
    let load = || async { Ok::<_, ()>("loaded".to_owned()) };

    redis.stop().await;

    for id in 1..=10 {
        let loaded = store.get_or_set(&key(id), load, None).await;

        assert_eq!(loaded.as_deref(), Ok("loaded"));
    }

    // Each call read twice, before and after its lock, and stored once.
    assert_eq!(
        metric("sisa.caching.errors", &[KeyValue::new("operation", "get")]),
        20
    );
    assert_eq!(
        metric("sisa.caching.errors", &[KeyValue::new("operation", "set")]),
        10
    );

    redis.restart().await;
    eventually("the store reconnects", || async {
        store.get::<String>(&key(1)).await.is_ok()
    })
    .await;
    // Both reads work, then storing what it loaded does.
    store.get_or_set(&key(1), load, None).await.unwrap();

    let lines: Vec<String> = logs
        .lines()
        .into_iter()
        .filter(|line| line.contains("operation="))
        .collect();
    let logged = |level: &str, operation: &str| {
        lines
            .iter()
            .filter(|line| {
                line.starts_with(level) && line.contains(&format!(r#"operation="{operation}""#))
            })
            .count()
    };

    assert_eq!(lines.len(), 4, "{lines:#?}");
    assert_eq!(logged("WARN", "get"), 1, "{lines:#?}");
    assert_eq!(logged("WARN", "set"), 1, "{lines:#?}");
    assert_eq!(logged("INFO", "get"), 1, "{lines:#?}");
    assert_eq!(logged("INFO", "set"), 1, "{lines:#?}");
}
