//! Hybrid nodes on a three-primary cluster, started in Docker, whose backplane channel moves to
//! another primary.

use std::time::Duration;

use crate::common::{fast_heartbeats, key, local_value, start_with};
use redis::cluster_async::ClusterConnection;
use sisa_caching_core::cache::CacheStore;
use sisa_caching_hybrid::{backplane::RedisBackplane, store::HybridCacheStore};
use sisa_caching_memory::store::InMemoryCacheStore;
use sisa_caching_redis::store::RedisCacheStore;
use sisa_caching_testing::{RedisCluster, eventually};

type Node = HybridCacheStore<RedisCacheStore<ClusterConnection>>;

async fn node(cluster: &RedisCluster) -> Node {
    start_with(
        HybridCacheStore::new(
            InMemoryCacheStore::new("users").unwrap(),
            RedisCacheStore::new("users", cluster.connection().await).unwrap(),
            RedisBackplane::cluster(cluster.client()).await.unwrap(),
        )
        .unwrap(),
        fast_heartbeats(),
    )
    .await
}

/// Whether a write of `user:1` through `writer` evicts the reader's copy of it within half a
/// second while its copy of `user:2` stays, so the write's invalidation reached the reader rather
/// than a clear. False while the reader makes no copies, as between subscriptions.
async fn an_invalidation_reaches(writer: &Node, reader: &Node) -> bool {
    for id in [1, 2] {
        writer
            .distributed_store()
            .set(&key(id), "old".to_owned(), None)
            .await
            .unwrap();
        reader.get::<String>(&key(id)).await.unwrap();

        if local_value(reader, &key(id)).await.is_none() {
            return false;
        }
    }

    writer.set(&key(1), "new".to_owned(), None).await.unwrap();

    let evicted = tokio::time::timeout(Duration::from_millis(500), async {
        while local_value(reader, &key(1)).await.is_some() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;

    evicted.is_ok() && local_value(reader, &key(2)).await.is_some()
}

#[tokio::test]
async fn invalidations_reach_other_nodes_after_the_channel_slot_moves() {
    let cluster = RedisCluster::start().await;
    let writer = node(&cluster).await;
    let reader = node(&cluster).await;

    assert!(an_invalidation_reaches(&writer, &reader).await);
    cluster.move_slot("users:backplane").await;

    // Redis and Valkey end the subscriptions on the old owner. Garnet keeps them there, where they
    // get nothing, until each node's own heartbeats stop coming back. Either way the nodes
    // subscribe again on the new owner.
    eventually("an invalidation reaches the reader again", || {
        an_invalidation_reaches(&writer, &reader)
    })
    .await;
}
