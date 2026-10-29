//! Hybrid nodes on a three-primary cluster, started in Docker for each test, with the backplane on
//! the cluster too.

mod common;

use std::time::Duration;

use common::{assert_a_write_reaches, start};
use sisa_caching_core::cache::KeyPrefix;
use sisa_caching_hybrid::{
    backplane::{
        Backplane, BackplaneMessage, BackplaneSubscription, Invalidation, NodeId, RedisBackplane,
    },
    settings::{DEFAULT_REDIS_CHANNEL, RedisBackplaneSettings},
    store::HybridCacheStore,
};
use sisa_caching_memory::store::InMemoryCacheStore;
use sisa_caching_redis::store::RedisCacheStore;
use sisa_caching_testing::RedisCluster;

async fn node(
    cluster: &RedisCluster,
) -> HybridCacheStore<RedisCacheStore<redis::cluster_async::ClusterConnection>, RedisBackplane> {
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

#[tokio::test]
async fn a_sharded_subscription_ends_only_when_its_channel_slot_moves() {
    let cluster = RedisCluster::start().await;
    let backplane = RedisBackplane::cluster(
        cluster.client(),
        RedisBackplaneSettings {
            ping_interval_ms: 100,
            ..RedisBackplaneSettings::default()
        },
    )
    .await
    .unwrap();
    let message = BackplaneMessage::new(
        NodeId::new("node-a"),
        KeyPrefix::new("users").unwrap(),
        Invalidation::Clear,
    );
    let mut subscription = backplane.subscribe().await.unwrap();

    // Waiting through a few pings while the slot stays put, the subscription still delivers.
    let (_, next) = tokio::join!(
        async {
            tokio::time::sleep(Duration::from_millis(350)).await;
            backplane.publish(&message).await.unwrap();
        },
        tokio::time::timeout(Duration::from_secs(5), subscription.next()),
    );

    next.expect("the message arrives").unwrap();

    cluster.move_slot(DEFAULT_REDIS_CHANNEL).await;
    backplane.publish(&message).await.unwrap();

    // Delivering the message is fine too; waiting on a node that no longer gets it is not.
    let next = tokio::time::timeout(Duration::from_secs(5), subscription.next())
        .await
        .expect("the subscription delivers the message or ends");
    if next.is_err() {
        let mut subscription = backplane.subscribe().await.unwrap();

        backplane.publish(&message).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), subscription.next())
            .await
            .expect("a new subscription gets the message")
            .unwrap();
    }
}

#[tokio::test]
async fn a_sharded_subscription_that_falls_too_far_behind_ends() {
    let cluster = RedisCluster::start().await;
    let backplane = RedisBackplane::cluster(cluster.client(), RedisBackplaneSettings::default())
        .await
        .unwrap();
    let mut subscription = backplane.subscribe().await.unwrap();
    let payload = serde_json::to_vec(&BackplaneMessage::new(
        NodeId::new("node-a"),
        KeyPrefix::new("users").unwrap(),
        Invalidation::Clear,
    ))
    .unwrap();
    let mut publish = redis::pipe();

    // More than the 8,192 messages a subscription holds, all read off the connection before
    // anything reads the subscription.
    for _ in 0..20_000 {
        publish
            .cmd("SPUBLISH")
            .arg(DEFAULT_REDIS_CHANNEL)
            .arg(&payload)
            .ignore();
    }
    publish
        .query_async::<()>(&mut cluster.connection().await)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;

    let error = tokio::time::timeout(Duration::from_secs(5), subscription.next())
        .await
        .expect("the subscription ends")
        .unwrap_err();

    assert!(error.to_string().contains("fell too far behind"), "{error}");
}
