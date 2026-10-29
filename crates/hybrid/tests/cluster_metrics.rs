//! `invalidations.delivered` on a cluster, where the backplane publishes with `SPUBLISH`. A test
//! binary of its own, so its meter provider is installed before any store is used.

mod common;

use common::start;
use opentelemetry::global;
use opentelemetry_sdk::metrics::{
    InMemoryMetricExporter, PeriodicReader, SdkMeterProvider,
    data::{AggregatedMetrics, MetricData},
};
use sisa_caching_core::cache::CacheStore;
use sisa_caching_hybrid::{
    backplane::RedisBackplane, settings::RedisBackplaneSettings, store::HybridCacheStore,
};
use sisa_caching_memory::store::InMemoryCacheStore;
use sisa_caching_redis::store::RedisCacheStore;
use sisa_caching_testing::{RedisCluster, eventually};

async fn node(
    cluster: &RedisCluster,
) -> HybridCacheStore<RedisCacheStore<redis::cluster_async::ClusterConnection>> {
    start(
        HybridCacheStore::new(
            InMemoryCacheStore::new("users").unwrap(),
            RedisCacheStore::new("users", cluster.connection().await).unwrap(),
            RedisBackplane::cluster(cluster.client(), RedisBackplaneSettings::default())
                .await
                .unwrap(),
        )
        .unwrap(),
    )
    .await
}

#[tokio::test]
async fn a_write_counts_every_subscriber_on_a_cluster() {
    let exporter = InMemoryMetricExporter::default();
    let provider = SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(exporter.clone()).build())
        .build();

    global::set_meter_provider(provider.clone());

    let delivered = || {
        provider.force_flush().unwrap();

        exporter
            .get_finished_metrics()
            .unwrap()
            .iter()
            .flat_map(|resource| resource.scope_metrics())
            .flat_map(|scope| scope.metrics())
            .filter(|metric| metric.name() == "sisa.caching.invalidations.delivered")
            .filter_map(|metric| match metric.data() {
                AggregatedMetrics::U64(MetricData::Sum(sum)) => {
                    sum.data_points().map(|point| point.value()).last()
                }
                _ => None,
            })
            .last()
            .unwrap_or_default()
    };
    let cluster = RedisCluster::start().await;
    let writer = node(&cluster).await;
    let _reader = node(&cluster).await;

    writer.set("user:1", "ada".to_owned(), None).await.unwrap();

    // Both subscriptions sit on the node that owns the channel's slot, which took the publish.
    eventually("the publish counts both subscribers", || async {
        delivered() == 2
    })
    .await;
}
