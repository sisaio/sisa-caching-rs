//! Hybrid nodes behind Sentinel, through a failover. Each test starts a primary, a replica and a
//! Sentinel in Docker.

use crate::common::{key, local_value, start};
use sisa_caching_core::cache::CacheStore;
use sisa_caching_hybrid::{
    backplane::RedisBackplane, settings::RedisBackplaneSettings, store::HybridCacheStore,
};
use sisa_caching_memory::store::InMemoryCacheStore;
use sisa_caching_redis::{connection::SentinelConnection, store::RedisCacheStore};
use sisa_caching_testing::{RedisSentinel, eventually};

type Node = HybridCacheStore<RedisCacheStore<SentinelConnection>>;

async fn connect(sentinel: &RedisSentinel) -> SentinelConnection {
    SentinelConnection::connect(sentinel.sentinel_client())
        .await
        .unwrap()
}

/// A node whose store and backplane share one Sentinel connection.
async fn node(sentinel: &RedisSentinel) -> Node {
    node_on(connect(sentinel).await).await
}

async fn node_on(conn: SentinelConnection) -> Node {
    start(
        HybridCacheStore::new(
            InMemoryCacheStore::new("users").unwrap(),
            RedisCacheStore::new("users", conn.clone()).unwrap(),
            RedisBackplane::sentinel(conn, {
                let mut settings = RedisBackplaneSettings::default();
                settings.ping_interval_ms = 100;
                settings
            }),
        )
        .unwrap(),
    )
    .await
}

async fn subscribers_on(sentinel: &RedisSentinel, port: u16) -> usize {
    let (_, count): (String, usize) = redis::cmd("PUBSUB")
        .arg("NUMSUB")
        .arg("users:backplane")
        .query_async(
            &mut sentinel
                .server(port)
                .get_multiplexed_async_connection()
                .await
                .unwrap(),
        )
        .await
        .unwrap();

    count
}

#[tokio::test]
async fn nodes_follow_a_failover() {
    let Some(sentinel) = RedisSentinel::start().await else {
        return;
    };
    let writer = node(&sentinel).await;
    let reader = node(&sentinel).await;
    // A third node's connection opens before the failover, and the node starts after it.
    let late = connect(&sentinel).await;

    sentinel.failover().await;
    sentinel.demote_old_primary().await;
    let _late_node = node_on(late.clone()).await;

    // Nothing failed on the old primary, so only subscribing moved the late node's connection.
    let role: Vec<redis::Value> = redis::cmd("ROLE")
        .query_async(&mut late.clone())
        .await
        .unwrap();
    assert_eq!(role[0], redis::Value::BulkString(b"master".to_vec()));

    // The demoted primary refuses this write, so the writer's connection moves to the new one.
    writer
        .distributed_store()
        .set(&key(1), "old".to_owned(), None)
        .await
        .unwrap();
    // Demoting by hand closes no connection, like a failover on Dragonfly, which refuses the
    // command Sentinel closes them with. The subscriptions' pings find the primary moved.
    eventually("every subscription moves to the new primary", || async {
        subscribers_on(&sentinel, 6379).await == 0 && subscribers_on(&sentinel, 6380).await == 3
    })
    .await;

    eventually("the reader copies the replicated value", || async {
        reader.get::<String>(&key(1)).await.unwrap().as_deref() == Some("old")
            && local_value(&reader, &key(1)).await.is_some()
    })
    .await;

    writer.set(&key(1), "new".to_owned(), None).await.unwrap();

    eventually("the reader drops its copy", || async {
        local_value(&reader, &key(1)).await.is_none()
    })
    .await;
    assert_eq!(
        reader.get::<String>(&key(1)).await.unwrap().as_deref(),
        Some("new")
    );
}
