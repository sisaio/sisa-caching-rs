//! Hybrid nodes behind Sentinel, through a failover. Each test starts a primary, a replica and a
//! Sentinel in Docker.

mod common;

use common::{key, local_value, start};
use sisa_caching_core::cache::CacheStore;
use sisa_caching_hybrid::{
    backplane::RedisBackplane,
    settings::{DEFAULT_REDIS_CHANNEL, RedisBackplaneSettings},
    store::HybridCacheStore,
};
use sisa_caching_memory::store::InMemoryCacheStore;
use sisa_caching_redis::{connection::SentinelConnection, store::RedisCacheStore};
use sisa_caching_testing::{RedisSentinel, eventually};

type Node = HybridCacheStore<RedisCacheStore<SentinelConnection>, RedisBackplane>;

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
            RedisBackplane::sentinel(
                conn,
                RedisBackplaneSettings {
                    ping_interval_ms: 100,
                    ..RedisBackplaneSettings::default()
                },
            ),
        )
        .unwrap(),
    )
    .await
}

async fn subscribers_on(sentinel: &RedisSentinel, port: u16) -> usize {
    let (_, count): (String, usize) = redis::cmd("PUBSUB")
        .arg("NUMSUB")
        .arg(DEFAULT_REDIS_CHANNEL)
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
async fn a_write_after_a_failover_evicts_the_copy_on_another_node() {
    let Some(sentinel) = RedisSentinel::start().await else {
        return;
    };
    let writer = node(&sentinel).await;
    let reader = node(&sentinel).await;

    sentinel.failover().await;
    sentinel.demote_old_primary().await;

    // The demoted primary refuses this write, so the writer's connection moves to the new one.
    writer
        .distributed_store()
        .set(&key(1), "old".to_owned(), None)
        .await
        .unwrap();
    // Demoting by hand closes no connection, like a failover on Dragonfly, which refuses the
    // command Sentinel closes them with. The subscriptions' pings find the primary moved.
    eventually("both subscriptions move to the new primary", || async {
        subscribers_on(&sentinel, 6379).await == 0 && subscribers_on(&sentinel, 6380).await == 2
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

#[tokio::test]
async fn a_node_started_after_a_failover_reads_from_the_primary_it_subscribed_to() {
    let Some(sentinel) = RedisSentinel::start().await else {
        return;
    };
    let conn = connect(&sentinel).await;

    sentinel.failover().await;
    sentinel.demote_old_primary().await;
    let _node = node_on(conn.clone()).await;

    // Nothing failed on the old primary, so only subscribing moved the shared connection.
    let role: Vec<redis::Value> = redis::cmd("ROLE")
        .query_async(&mut conn.clone())
        .await
        .unwrap();
    assert_eq!(role[0], redis::Value::BulkString(b"master".to_vec()));
    assert_eq!(subscribers_on(&sentinel, 6380).await, 1);
}
