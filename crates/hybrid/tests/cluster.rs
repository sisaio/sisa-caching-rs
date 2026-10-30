//! Hybrid nodes on a three-primary cluster, started in Docker, with the backplane on the cluster
//! too, where it publishes with `SPUBLISH`. A test binary of its own, so its meter provider is
//! installed before any store is built.

mod common;

use common::{assert_a_write_reaches, start};
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
async fn a_write_evicts_the_copy_on_another_node_and_counts_both_subscribers() {
    let metrics = Metrics::install();
    let cluster = RedisCluster::start().await;

    assert_a_write_reaches(&node(&cluster).await, &node(&cluster).await).await;

    // Both subscriptions sit on the node that owns the channel's slot, which took the publish.
    eventually("the publish counts both subscribers", || async {
        metrics.value("sisa.caching.invalidations.delivered", &[]) == 2
    })
    .await;
}
