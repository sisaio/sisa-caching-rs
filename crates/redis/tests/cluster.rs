//! The Redis store on a three-primary Redis Cluster, started in Docker for each test.

use redis::Client;
use sisa_caching_core::cache::{CacheKey, CacheStore};
use sisa_caching_redis::store::RedisCacheStore;
use sisa_caching_testing::RedisCluster;

fn key(kind: &str, key: u32) -> CacheKey {
    CacheKey::new(kind, key).unwrap()
}

async fn key_count(node: &Client) -> usize {
    redis::cmd("DBSIZE")
        .query_async(&mut node.get_multiplexed_async_connection().await.unwrap())
        .await
        .unwrap()
}

#[tokio::test]
async fn remove_prefix_and_clear_reach_every_primary() {
    remove_prefix_and_clear_reach_every_node_of(&RedisCluster::start().await).await;
}

#[tokio::test]
async fn remove_prefix_and_clear_reach_nodes_that_report_no_endpoint() {
    let Some(cluster) = RedisCluster::start_without_endpoints().await else {
        return;
    };

    remove_prefix_and_clear_reach_every_node_of(&cluster).await;
}

async fn remove_prefix_and_clear_reach_every_node_of(cluster: &RedisCluster) {
    let store = RedisCacheStore::new("app", cluster.connection().await).unwrap();

    for id in 0..30 {
        store.set(&key("user", id), id, None).await.unwrap();
    }
    store.set(&key("order", 1), 1, None).await.unwrap();

    for node in cluster.nodes() {
        assert!(key_count(&node).await > 0, "every primary holds some keys");
    }

    let removed = store.remove_prefix("user").await.unwrap();

    assert_eq!(removed, 30);
    for id in 0..30 {
        assert_eq!(store.get::<u32>(&key("user", id)).await.unwrap(), None);
    }
    assert_eq!(store.get::<u32>(&key("order", 1)).await.unwrap(), Some(1));

    store.clear().await.unwrap();

    for node in cluster.nodes() {
        assert_eq!(key_count(&node).await, 0);
    }
}
