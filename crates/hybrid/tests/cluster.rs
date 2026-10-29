//! Hybrid nodes on a three-primary cluster, started in Docker for each test, with the backplane on
//! the cluster too.

mod common;

use common::{assert_a_write_reaches, start};
use sisa_caching_hybrid::{
    backplane::RedisBackplane, settings::RedisBackplaneSettings, store::HybridCacheStore,
};
use sisa_caching_memory::store::InMemoryCacheStore;
use sisa_caching_redis::store::RedisCacheStore;
use sisa_caching_testing::RedisCluster;

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
async fn a_write_evicts_the_copy_on_another_node() {
    let cluster = RedisCluster::start().await;

    assert_a_write_reaches(&node(&cluster).await, &node(&cluster).await).await;
}
