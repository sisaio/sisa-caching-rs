//! `invalidations.delivered` on a cluster, where the backplane publishes with `SPUBLISH`. A test
//! binary of its own, so its meter provider is installed before any store is built.

mod common;

use common::start;
use sisa_caching_core::cache::CacheStore;
use sisa_caching_hybrid::{
    backplane::RedisBackplane, settings::RedisBackplaneSettings, store::HybridCacheStore,
};
use sisa_caching_memory::store::InMemoryCacheStore;
use sisa_caching_redis::store::RedisCacheStore;
use sisa_caching_testing::{Metrics, RedisCluster, eventually};

async fn node(
    cluster: &RedisCluster,
) -> HybridCacheStore<RedisCacheStore<redis::cluster_async::ClusterConnection>> {
    start(
        HybridCacheStore::new(
            InMemoryCacheStore::new("app").unwrap(),
            RedisCacheStore::new("app", cluster.connection().await).unwrap(),
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
    let metrics = Metrics::install();
    let cluster = RedisCluster::start().await;
    let writer = node(&cluster).await;
    let _reader = node(&cluster).await;

    writer.set("user:1", "ada".to_owned(), None).await.unwrap();

    // Both subscriptions sit on the node that owns the channel's slot, which took the publish.
    eventually("the publish counts both subscribers", || async {
        metrics.value("sisa.caching.invalidations.delivered", &[]) == 2
    })
    .await;
}
