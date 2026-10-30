//! The Redis store against a real server: each test starts its own in Docker.

use std::time::{Duration, Instant, SystemTime};

use redis::{
    Cmd, Pipeline, RedisFuture, Value,
    aio::{ConnectionLike, ConnectionManager},
};
use serde::{Deserialize, Serialize};
use sisa_caching_core::{
    cache::{CacheEntry, CacheKey, CacheOptions, CacheStore},
    clock::{Clock, SystemClock},
};
use sisa_caching_redis::{
    connection::RedisConnection, error::RedisCacheError, settings::RedisCacheSettings,
    store::RedisCacheStore,
};
use sisa_caching_testing::{Redis, eventually};

fn key(key: u32) -> CacheKey {
    CacheKey::new("user", key).unwrap()
}

fn ttl(ttl: Duration) -> CacheOptions {
    CacheOptions::with_ttl(ttl)
}

async fn store(redis: &Redis) -> RedisCacheStore {
    RedisCacheStore::connect("app", redis.url()).await.unwrap()
}

async fn pttl(redis: &Redis, redis_key: &str) -> i64 {
    redis::cmd("PTTL")
        .arg(redis_key)
        .query_async(&mut redis.connection().await)
        .await
        .unwrap()
}

#[tokio::test]
async fn the_key_expires_when_the_entry_does() {
    let redis = Redis::start().await;
    let store = store(&redis).await;
    let now = SystemClock.now();
    let ten_secs = Duration::from_secs(10);
    let entry = |ttl| CacheEntry::new("ada".to_owned(), now, ttl);
    let until = |deadline| entry(None).with_expires_at(Some(deadline));

    // Each entry, the TTL it reads back with, and its key's PTTL: -1 is no expiry.
    for (id, written, read_ttl, expiry) in [
        (1, entry(Some(ten_secs)), Some(ten_secs), 9_000..=10_000),
        (2, entry(None), None, -1..=-1),
        // Too long for Redis, so it is stored without one.
        (3, entry(Some(Duration::MAX)), None, -1..=-1),
        // A deadline but no TTL: the key expires at the deadline.
        (4, until(now + ten_secs), None, 9_000..=10_000),
    ] {
        store.set_entry(&key(id), written).await.unwrap();

        let stored = store.get_entry::<String>(&key(id)).await.unwrap().unwrap();
        let left = pttl(&redis, &format!("app::user:{id}")).await;

        assert_eq!(stored.value, "ada", "{id}");
        assert_eq!(stored.ttl(), read_ttl, "{id}");
        assert!(expiry.contains(&left), "{id}: PTTL {left}");
    }

    store.set(&key(5), "old".to_owned(), None).await.unwrap();
    store
        .set_entry(&key(5), until(now - Duration::from_secs(1)))
        .await
        .unwrap();

    assert_eq!(
        pttl(&redis, "app::user:5").await,
        -2,
        "already expired, so removed"
    );
}

#[tokio::test]
async fn debug_shows_the_store_but_never_its_connection_or_a_value() {
    let redis = Redis::start().await;
    let store = store(&redis).await;

    store.set(&key(1), "secret".to_owned(), None).await.unwrap();

    let shown = format!("{store:?}");

    assert!(shown.contains(r#"name: KeyPrefix("app")"#), "{shown}");
    assert!(!shown.contains("secret"), "{shown}");
    assert!(!shown.contains(redis.url()), "{shown}");
}

#[tokio::test]
async fn compatible_json_reads_as_another_type_and_incompatible_json_is_an_error() {
    let redis = Redis::start().await;
    let store = store(&redis).await;

    store.set(&key(1), 7_u64, None).await.unwrap();

    assert_eq!(store.get::<i64>(&key(1)).await.unwrap(), Some(7));
    assert!(matches!(
        store.get::<Vec<String>>(&key(1)).await,
        Err(RedisCacheError::Json(_))
    ));
}

#[tokio::test]
async fn remove_prefix_removes_whole_levels_on_every_scan_page() {
    let redis = Redis::start().await;
    // A small batch, so SCAN needs many pages.
    let store = store(&redis)
        .await
        .with_settings(RedisCacheSettings {
            scan_batch_size: 5,
            ..RedisCacheSettings::default()
        })
        .unwrap();
    let v1 = |id| CacheKey::new("user:v1", id).unwrap();
    let v2 = CacheKey::new("user:v2", 1).unwrap();
    let username = CacheKey::new("username", 1).unwrap();

    for id in 0..50 {
        store.set(&v1(id), id, None).await.unwrap();
    }
    store.set(&v2, 2_u32, None).await.unwrap();
    store.set(&username, 1_u32, None).await.unwrap();

    assert_eq!(
        store.remove_prefix("u*").await.unwrap(),
        0,
        "`*` in a prefix is a plain character"
    );
    assert_eq!(store.remove_prefix("user:v1").await.unwrap(), 50);
    assert_eq!(store.get::<u32>(&v2).await.unwrap(), Some(2));
    assert_eq!(
        store.remove_prefix("user").await.unwrap(),
        1,
        "the name drops the other version too"
    );
    assert_eq!(
        store.get::<u32>(&username).await.unwrap(),
        Some(1),
        "`user` does not match `username`"
    );
}

#[tokio::test]
async fn clear_keeps_the_keys_of_other_stores() {
    let redis = Redis::start().await;
    let store = store(&redis).await;
    // Its name starts with the first store's name.
    let other = RedisCacheStore::new("app-other", redis.connection().await).unwrap();

    store.set(&key(1), 1_u32, None).await.unwrap();
    other.set(&key(1), 2_u32, None).await.unwrap();
    store.clear().await.unwrap();

    assert_eq!(store.get::<u32>(&key(1)).await.unwrap(), None);
    assert_eq!(other.get::<u32>(&key(1)).await.unwrap(), Some(2));
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct User {
    id: u64,
    name: String,
}

#[tokio::test]
async fn entries_are_stored_as_the_documented_hash() {
    let redis = Redis::start().await;
    let store = store(&redis).await;
    let mut conn = redis.connection().await;
    let now_ms = SystemClock
        .now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    // Written as another language would.
    redis::pipe()
        .cmd("HSET")
        .arg("app::user:1")
        .arg("value")
        .arg(r#"{"id":1,"name":"Ada"}"#)
        .arg("created_at")
        .arg(now_ms)
        .arg("ttl_ms")
        .arg(30_000)
        .ignore()
        .cmd("PEXPIRE")
        .arg("app::user:1")
        .arg(30_000)
        .ignore()
        .query_async::<()>(&mut conn)
        .await
        .unwrap();

    let ada = User {
        id: 1,
        name: "Ada".to_owned(),
    };
    let entry = store.get_entry::<User>(&key(1)).await.unwrap().unwrap();

    assert_eq!(entry.value, ada);
    assert_eq!(entry.ttl(), Some(Duration::from_secs(30)));

    // And written by this store.
    store
        .set(&key(2), ada, ttl(Duration::from_secs(30)))
        .await
        .unwrap();

    let mut fields: Vec<String> = redis::cmd("HKEYS")
        .arg("app::user:2")
        .query_async(&mut conn)
        .await
        .unwrap();
    let value: String = redis::cmd("HGET")
        .arg("app::user:2")
        .arg("value")
        .query_async(&mut conn)
        .await
        .unwrap();

    fields.sort();
    assert_eq!(fields, ["created_at", "ttl_ms", "value"]);
    assert_eq!(value, r#"{"id":1,"name":"Ada"}"#);
}

/// A connection whose replies arrive `delay` late.
#[derive(Clone)]
struct SlowReplies {
    inner: ConnectionManager,
    delay: Duration,
}

impl ConnectionLike for SlowReplies {
    fn req_packed_command<'a>(&'a mut self, cmd: &'a Cmd) -> RedisFuture<'a, Value> {
        Box::pin(async move {
            let reply = self.inner.req_packed_command(cmd).await;

            tokio::time::sleep(self.delay).await;
            reply
        })
    }

    fn req_packed_commands<'a>(
        &'a mut self,
        pipe: &'a Pipeline,
        offset: usize,
        count: usize,
    ) -> RedisFuture<'a, Vec<Value>> {
        Box::pin(async move {
            let replies = self.inner.req_packed_commands(pipe, offset, count).await;

            tokio::time::sleep(self.delay).await;
            replies
        })
    }

    fn get_db(&self) -> i64 {
        self.inner.get_db()
    }
}

impl RedisConnection for SlowReplies {}

#[tokio::test]
async fn a_late_reply_never_returns_an_expired_value() {
    let redis = Redis::start().await;
    let store = store(&redis).await;
    let slow = RedisCacheStore::new(
        "app",
        SlowReplies {
            inner: redis.connection().await,
            delay: Duration::from_millis(200),
        },
    )
    .unwrap();

    store
        .set(&key(1), "ada".to_owned(), ttl(Duration::from_millis(50)))
        .await
        .unwrap();

    assert_eq!(slow.get::<String>(&key(1)).await.unwrap(), None);
}

/// Takes `100 ms` to encode, as a large value might.
#[derive(Clone, Deserialize)]
struct SlowToEncode(String);

impl Serialize for SlowToEncode {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        std::thread::sleep(Duration::from_millis(100));
        serializer.serialize_str(&self.0)
    }
}

/// An entry that lives 50 ms from now.
fn short<CV>(value: CV) -> CacheEntry<CV> {
    CacheEntry::new(value, SystemClock.now(), Some(Duration::from_millis(50)))
}

#[tokio::test]
async fn an_entry_expires_on_time_however_late_its_write_starts() {
    let redis = Redis::start().await;
    let store = store(&redis).await;
    // Polled after the entry has expired.
    let late = key(1);
    let write = store.set_entry(&late, short("ada".to_owned()));

    tokio::time::sleep(Duration::from_millis(100)).await;
    write.await.unwrap();

    // Encoded for longer than the entry lives.
    store
        .set_entry(&key(2), short(SlowToEncode("ada".to_owned())))
        .await
        .unwrap();

    assert_eq!(store.get::<String>(&key(1)).await.unwrap(), None);
    assert!(store.get::<SlowToEncode>(&key(2)).await.unwrap().is_none());
}

#[tokio::test]
async fn calls_fail_at_once_while_the_server_is_down_and_work_once_it_is_back() {
    let redis = Redis::start_restartable().await;
    let store = store(&redis).await;

    store.set(&key(1), "ada".to_owned(), None).await.unwrap();
    redis.stop().await;

    // redis-rs's default reconnect attempts would hold each call for about 9 s.
    let started = Instant::now();

    for _ in 0..3 {
        assert!(store.get::<String>(&key(1)).await.is_err());
    }

    let loaded = store
        .get_or_set(&key(1), || async { Ok::<_, ()>("loaded".to_owned()) }, None)
        .await;

    assert_eq!(loaded.as_deref(), Ok("loaded"));
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "took {:?}",
        started.elapsed()
    );

    redis.restart().await;

    eventually("the store reconnects", || async {
        store.set(&key(1), "grace".to_owned(), None).await.is_ok()
    })
    .await;
    assert_eq!(
        store.get::<String>(&key(1)).await.unwrap().as_deref(),
        Some("grace")
    );
}
