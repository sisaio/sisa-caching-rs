//! Hybrid nodes on a real Redis: memory local layers, the Redis store, and the Redis pub/sub
//! backplane. Each test starts its own server in Docker.

mod common;

use std::time::{Duration, SystemTime};

use common::{assert_a_write_reaches, key, local_value, start};
use sisa_caching_core::{
    cache::{CacheEntry, CacheStore, KeyPrefix},
    clock::{Clock, SystemClock},
    settings::CacheSettings,
};
use sisa_caching_hybrid::{
    backplane::{
        Backplane, BackplaneMessage, BackplaneSubscription, Invalidation, NodeId, RedisBackplane,
    },
    settings::{DEFAULT_REDIS_CHANNEL, HybridCacheSettings, RedisBackplaneSettings},
    store::{HybridCacheStore, RedisHybridCacheStore},
};
use sisa_caching_memory::{settings::InMemoryCacheSettings, store::InMemoryCacheStore};
use sisa_caching_redis::{settings::RedisCacheSettings, store::RedisCacheStore};
use sisa_caching_testing::{Redis, eventually};

async fn connect_backplane(redis: &Redis, settings: RedisBackplaneSettings) -> RedisBackplane {
    RedisBackplane::connect(redis.url(), settings)
        .await
        .unwrap()
}

/// A node for `store`, with its own connections, as in a process of its own.
async fn node_with(
    redis: &Redis,
    store: &str,
    settings: RedisBackplaneSettings,
) -> RedisHybridCacheStore {
    start(
        HybridCacheStore::new(
            InMemoryCacheStore::new(store).unwrap(),
            RedisCacheStore::new(store, redis.connection().await).unwrap(),
            connect_backplane(redis, settings).await,
        )
        .unwrap(),
    )
    .await
}

async fn node(redis: &Redis) -> RedisHybridCacheStore {
    node_with(redis, "users", RedisBackplaneSettings::default()).await
}

/// Pings every 50 ms and gives up after 100 ms, so a stall shows quickly.
fn quick_pings() -> RedisBackplaneSettings {
    RedisBackplaneSettings {
        ping_interval_ms: 50,
        ping_timeout_ms: 100,
        ..RedisBackplaneSettings::default()
    }
}

#[tokio::test]
async fn connect_builds_every_part_on_one_server_with_its_settings() {
    let redis = Redis::start().await;
    let settings = HybridCacheSettings {
        local: InMemoryCacheSettings {
            max_ttl_secs: Some(60),
            ..InMemoryCacheSettings::default()
        },
        distributed: RedisCacheSettings {
            common: CacheSettings {
                ttl_secs: Some(300),
                ..CacheSettings::default()
            },
            ..RedisCacheSettings::default()
        },
        ..HybridCacheSettings::default()
    };
    let connect = || HybridCacheStore::connect("users", redis.url(), &settings);
    let writer = start(connect().await.unwrap()).await;
    let reader = start(connect().await.unwrap()).await;

    assert_eq!(reader.local_store().settings(), &settings.local);
    assert_eq!(reader.distributed_store().settings(), &settings.distributed);

    assert_a_write_reaches(&writer, &reader).await;

    let entry = reader.get_entry::<String>(&key(1)).await.unwrap().unwrap();

    assert_eq!(entry.ttl(), Some(Duration::from_secs(300)));
}

#[tokio::test]
async fn stores_sharing_a_channel_get_only_their_own_invalidations() {
    let redis = Redis::start().await;
    let users_writer = node_with(&redis, "users", RedisBackplaneSettings::default()).await;
    let users_reader = node_with(&redis, "users", RedisBackplaneSettings::default()).await;
    let teams_reader = node_with(&redis, "teams", RedisBackplaneSettings::default()).await;

    for reader in [&users_reader, &teams_reader] {
        reader
            .distributed_store()
            .set(&key(1), "old".to_owned(), None)
            .await
            .unwrap();
        reader.get::<String>(&key(1)).await.unwrap();
    }

    users_writer.remove(&key(1)).await.unwrap();

    eventually("the users reader drops its copy", || async {
        local_value(&users_reader, &key(1)).await.is_none()
    })
    .await;
    assert_eq!(
        local_value(&teams_reader, &key(1)).await.as_deref(),
        Some("old")
    );
}

#[tokio::test]
async fn the_redis_ttl_decides_when_a_copy_expires_whatever_the_writers_clock() {
    let redis = Redis::start().await;
    let node = node(&redis).await;

    // Written by a node whose clock ran far behind: by its `created_at`, the 60 s entry expired
    // long ago, but Redis still holds it for 60 s.
    redis::pipe()
        .cmd("HSET")
        .arg("users::user:1")
        .arg("value")
        .arg(r#""ada""#)
        .arg("created_at")
        .arg(1_000)
        .arg("ttl_ms")
        .arg(60_000)
        .ignore()
        .cmd("PEXPIRE")
        .arg("users::user:1")
        .arg(60_000)
        .ignore()
        .query_async::<()>(&mut redis.connection().await)
        .await
        .unwrap();

    assert_eq!(
        node.get::<String>(&key(1)).await.unwrap().as_deref(),
        Some("ada")
    );

    let copy = node
        .local_store()
        .get_entry::<String>(&key(1))
        .await
        .unwrap()
        .unwrap();
    let left = copy.remaining_ttl(SystemClock.now()).unwrap();

    assert!(
        left > Duration::from_secs(55) && left <= Duration::from_secs(60),
        "{left:?}"
    );
}

#[tokio::test]
async fn an_entry_copies_unchanged_between_memory_redis_and_hybrid() {
    let redis = Redis::start().await;
    let memory = InMemoryCacheStore::new("users").unwrap();
    let distributed = RedisCacheStore::new("users", redis.connection().await).unwrap();
    let hybrid = node(&redis).await;
    // Created ten minutes ago with an hour's TTL, in whole milliseconds as Redis keeps them.
    let now_ms = SystemClock
        .now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let created_at = SystemTime::UNIX_EPOCH + Duration::from_millis(now_ms - 600_000);
    let entry = CacheEntry::new(
        "ada".to_owned(),
        created_at,
        Some(Duration::from_secs(3600)),
    );
    let fields =
        |entry: &CacheEntry<String>| (entry.value.clone(), entry.created_at(), entry.ttl());

    memory.set_entry(&key(1), entry.clone()).await.unwrap();

    let from_memory = memory.get_entry::<String>(&key(1)).await.unwrap().unwrap();

    distributed.set_entry(&key(2), from_memory).await.unwrap();

    let from_redis = distributed
        .get_entry::<String>(&key(2))
        .await
        .unwrap()
        .unwrap();

    hybrid.set_entry(&key(3), from_redis.clone()).await.unwrap();

    let from_hybrid = hybrid.get_entry::<String>(&key(3)).await.unwrap().unwrap();
    let left = from_hybrid.remaining_ttl(SystemClock.now()).unwrap();

    assert_eq!(fields(&from_redis), fields(&entry));
    assert_eq!(fields(&from_hybrid), fields(&entry));
    assert!(
        left > Duration::from_secs(2990) && left <= Duration::from_secs(3000),
        "{left:?}"
    );
}

#[tokio::test]
async fn a_local_copy_of_another_type_falls_back_to_redis_decoding() {
    let redis = Redis::start().await;
    let node = node(&redis).await;

    node.set(&key(1), 7_u64, None).await.unwrap();
    assert_eq!(
        node.get::<u64>(&key(1)).await.unwrap(),
        Some(7),
        "copied as u64"
    );

    // The u64 copy doesn't fit, so Redis decodes the JSON: not as a list, which keeps the copy,
    // but as an i64.
    assert!(node.get::<Vec<String>>(&key(1)).await.is_err());
    assert_eq!(
        node.local_store().get::<u64>(&key(1)).await.unwrap(),
        Some(7),
        "kept"
    );
    assert_eq!(node.get::<i64>(&key(1)).await.unwrap(), Some(7));
}

fn message(invalidation: Invalidation) -> BackplaneMessage {
    BackplaneMessage::new(
        NodeId::new("node-a"),
        KeyPrefix::new("users").unwrap(),
        invalidation,
    )
}

async fn next(subscription: &mut impl BackplaneSubscription) -> Result<BackplaneMessage, String> {
    tokio::time::timeout(Duration::from_secs(5), subscription.next())
        .await
        .expect("a message arrives")
        .map_err(|error| error.to_string())
}

#[tokio::test]
async fn every_subscription_receives_the_messages_of_its_channel() {
    let redis = Redis::start().await;
    let backplane = connect_backplane(&redis, RedisBackplaneSettings::default()).await;
    let elsewhere = connect_backplane(
        &redis,
        RedisBackplaneSettings {
            channel: "elsewhere".to_owned(),
            ..RedisBackplaneSettings::default()
        },
    )
    .await;
    let mut first = backplane.subscribe().await.unwrap();
    let mut second = backplane.subscribe().await.unwrap();
    let sent = [
        message(Invalidation::Key(key(1).into())),
        message(Invalidation::Prefix(KeyPrefix::new("user").unwrap())),
        message(Invalidation::Clear),
    ];

    elsewhere
        .publish(&message(Invalidation::Clear))
        .await
        .unwrap();
    for message in &sent {
        backplane.publish(message).await.unwrap();
    }

    for subscription in [&mut first, &mut second] {
        for message in &sent {
            assert_eq!(next(subscription).await.as_ref(), Ok(message));
        }
    }
}

#[tokio::test]
async fn an_unreadable_message_is_an_error_and_delivery_goes_on() {
    let redis = Redis::start().await;
    let backplane = connect_backplane(&redis, RedisBackplaneSettings::default()).await;
    let mut subscription = backplane.subscribe().await.unwrap();
    let mut conn = redis.connection().await;

    for payload in [
        "not json",
        r#"{"v":2,"source":"node-a","store":"users","kind":"clear"}"#,
        r#"{"v":1,"source":"node-a","store":"users","kind":"prefix","target":"user::eu"}"#,
    ] {
        redis::cmd("PUBLISH")
            .arg(DEFAULT_REDIS_CHANNEL)
            .arg(payload)
            .query_async::<usize>(&mut conn)
            .await
            .unwrap();

        assert!(next(&mut subscription).await.is_err(), "{payload}");
    }

    let sent = message(Invalidation::Clear);

    backplane.publish(&sent).await.unwrap();
    assert_eq!(next(&mut subscription).await, Ok(sent));
}

#[tokio::test]
async fn subscribing_to_a_server_that_does_not_answer_times_out() {
    let redis = Redis::start().await;
    let backplane = connect_backplane(&redis, quick_pings()).await;

    redis.pause().await;

    let attempt = tokio::time::timeout(Duration::from_secs(2), backplane.subscribe()).await;

    redis.unpause().await;
    assert!(attempt.expect("the ping timeout ends the attempt").is_err());
}

#[tokio::test]
async fn a_stalled_subscription_turns_local_reads_off_until_it_is_back() {
    let redis = Redis::start().await;
    let node = node_with(&redis, "users", quick_pings()).await;

    node.set(&key(1), "ada".to_owned(), None).await.unwrap();
    node.get::<String>(&key(1)).await.unwrap();

    // Connections stay open but nothing answers, as on a network that drops traffic silently.
    redis.pause().await;
    eventually("the node notices the stall", || async {
        !node.local_reads_enabled()
    })
    .await;
    assert_eq!(
        local_value(&node, &key(1)).await,
        None,
        "the stall clears the copies"
    );

    redis.unpause().await;
    eventually("the node subscribes again", || async {
        node.local_reads_enabled()
    })
    .await;
    node.get::<String>(&key(1)).await.unwrap();
    assert_eq!(local_value(&node, &key(1)).await.as_deref(), Some("ada"));
}

#[tokio::test]
async fn a_closed_subscription_clears_the_copies_and_subscribes_again() {
    let redis = Redis::start().await;
    let writer = node(&redis).await;
    let reader = node(&redis).await;

    // Written straight to Redis, so no invalidation is on its way, and only a closed
    // subscription clears these copies.
    writer
        .distributed_store()
        .set(&key(1), "old".to_owned(), None)
        .await
        .unwrap();
    for node in [&writer, &reader] {
        node.get::<String>(&key(1)).await.unwrap();
        assert_eq!(local_value(node, &key(1)).await.as_deref(), Some("old"));
    }

    assert_eq!(
        redis.drop_subscriptions().await,
        2,
        "one subscription per node"
    );
    // A node can see its connection close a while after the kill returns, so each wait is for
    // something the nodes did: clearing their copies once they see it, then subscribing again.
    eventually("both nodes clear their copies", || async {
        local_value(&writer, &key(1)).await.is_none()
            && local_value(&reader, &key(1)).await.is_none()
    })
    .await;
    eventually("both nodes subscribe again", || async {
        reader.local_reads_enabled() && writer.local_reads_enabled()
    })
    .await;

    assert_a_write_reaches(&writer, &reader).await;
}
