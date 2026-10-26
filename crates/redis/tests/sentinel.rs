//! The Redis store behind Sentinel, through a failover. Each test starts a primary, a replica and
//! a Sentinel in Docker.

use sisa_caching_core::cache::{CacheKey, CacheStore};
use sisa_caching_redis::{connection::SentinelConnection, store::RedisCacheStore};
use sisa_caching_testing::RedisSentinel;

fn key(key: u32) -> CacheKey {
    CacheKey::new("user", key).unwrap()
}

async fn connect(sentinel: &RedisSentinel) -> SentinelConnection {
    SentinelConnection::connect(sentinel.sentinel_client())
        .await
        .unwrap()
}

async fn value_on(sentinel: &RedisSentinel, port: u16, redis_key: &str) -> Option<String> {
    redis::cmd("HGET")
        .arg(redis_key)
        .arg("value")
        .query_async(
            &mut sentinel
                .server(port)
                .get_multiplexed_async_connection()
                .await
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn a_write_the_old_primary_refuses_moves_to_the_new_one() {
    let Some(sentinel) = RedisSentinel::start().await else {
        return;
    };
    let store = RedisCacheStore::new("app", connect(&sentinel).await).unwrap();

    store.set(&key(1), "ada".to_owned(), None).await.unwrap();
    sentinel.failover().await;
    sentinel.demote_old_primary().await;
    store.set(&key(2), "grace".to_owned(), None).await.unwrap();

    assert_eq!(
        value_on(&sentinel, 6380, "app::user:2").await.as_deref(),
        Some(r#""grace""#)
    );
    assert_eq!(
        store.get::<String>(&key(1)).await.unwrap().as_deref(),
        Some("ada")
    );
}

#[tokio::test]
async fn a_remove_the_old_primary_refuses_moves_to_the_new_one() {
    let Some(sentinel) = RedisSentinel::start().await else {
        return;
    };
    let store = RedisCacheStore::new("app", connect(&sentinel).await).unwrap();

    store.set(&key(1), "ada".to_owned(), None).await.unwrap();
    sentinel.failover().await;
    sentinel.demote_old_primary().await;
    store.remove(&key(1)).await.unwrap();

    assert_eq!(value_on(&sentinel, 6380, "app::user:1").await, None);
}

#[tokio::test]
async fn a_call_cut_off_during_a_failover_fails_and_the_next_one_reaches_the_new_primary() {
    let Some(sentinel) = RedisSentinel::start().await else {
        return;
    };
    let store = RedisCacheStore::new("app", connect(&sentinel).await).unwrap();

    store.set(&key(1), "ada".to_owned(), None).await.unwrap();
    sentinel.failover().await;
    sentinel.demote_old_primary().await;
    // The old primary drops the store's connection, so the call may or may not have run there.
    redis::cmd("CLIENT")
        .arg("KILL")
        .arg("TYPE")
        .arg("normal")
        .query_async::<usize>(
            &mut sentinel
                .server(6379)
                .get_multiplexed_async_connection()
                .await
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(store.remove(&key(1)).await.is_err(), "not sent again");
    assert!(store.remove(&key(1)).await.unwrap());
    assert_eq!(value_on(&sentinel, 6380, "app::user:1").await, None);
}

#[tokio::test]
async fn primary_client_moves_to_the_new_primary_without_a_failed_command() {
    let Some(sentinel) = RedisSentinel::start().await else {
        return;
    };
    let conn = connect(&sentinel).await;

    sentinel.failover().await;
    let primary = conn.primary_client().await.unwrap();

    assert_eq!(
        primary.get_connection_info().addr().to_string(),
        sentinel
            .server(6380)
            .get_connection_info()
            .addr()
            .to_string()
    );
}
