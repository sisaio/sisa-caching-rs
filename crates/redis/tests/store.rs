//! The Redis store against a real server: each test starts its own in Docker.

use std::time::{Duration, Instant, SystemTime};

use redis::{
    Cmd, Pipeline, RedisFuture, Value,
    aio::{ConnectionLike, ConnectionManager},
};
use serde::{Deserialize, Serialize};
use sisa_caching_core::{
    cache::{CacheEntry, CacheError, CacheKey, CacheOptions, CacheStore},
    clock::{Clock, SystemClock},
    settings::{CacheSettings, MAX_TTL_SECS},
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
async fn the_key_expires_with_the_entry() {
    let redis = Redis::start().await;
    let store = store(&redis).await;

    store
        .set(&key(1), "ada".to_owned(), ttl(Duration::from_secs(10)))
        .await
        .unwrap();
    store.set(&key(2), "grace".to_owned(), None).await.unwrap();

    let entry = store.get_entry::<String>(&key(1)).await.unwrap().unwrap();
    let expiry = pttl(&redis, "app::user:1").await;

    assert_eq!(entry.value, "ada");
    assert_eq!(entry.ttl(), Some(Duration::from_secs(10)));
    assert!((9_000..=10_000).contains(&expiry), "PTTL {expiry}");

    let forever = store.get_entry::<String>(&key(2)).await.unwrap().unwrap();

    assert_eq!(forever.ttl(), None);
    assert_eq!(pttl(&redis, "app::user:2").await, -1, "no expiry");
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
async fn a_ttl_too_long_for_redis_is_stored_as_none() {
    let redis = Redis::start().await;
    let store = store(&redis).await;
    let entry = CacheEntry::new("ada".to_owned(), SystemClock.now(), Some(Duration::MAX));

    store.set_entry(&key(1), entry).await.unwrap();

    let stored = store.get_entry::<String>(&key(1)).await.unwrap().unwrap();

    assert_eq!(stored.value, "ada");
    assert_eq!(stored.ttl(), None);
    assert_eq!(pttl(&redis, "app::user:1").await, -1, "no expiry");
}

#[tokio::test]
async fn an_entry_with_a_deadline_but_no_ttl_expires_at_its_deadline() {
    let redis = Redis::start().await;
    let store = store(&redis).await;
    let now = SystemClock.now();
    let until =
        |deadline| CacheEntry::new("ada".to_owned(), now, None).with_expires_at(Some(deadline));

    store
        .set_entry(&key(1), until(now + Duration::from_secs(10)))
        .await
        .unwrap();
    store.set(&key(2), "old".to_owned(), None).await.unwrap();
    store
        .set_entry(&key(2), until(now - Duration::from_secs(1)))
        .await
        .unwrap();

    let expiry = pttl(&redis, "app::user:1").await;

    assert!((9_000..=10_000).contains(&expiry), "PTTL {expiry}");
    assert_eq!(
        pttl(&redis, "app::user:2").await,
        -2,
        "already expired, so removed"
    );
}

#[tokio::test]
async fn a_ttl_over_365_days_is_rejected_before_anything_reaches_redis() {
    let redis = Redis::start().await;
    let store = store(&redis).await;
    let too_long = RedisCacheSettings {
        common: CacheSettings {
            ttl_secs: Some(MAX_TTL_SECS + 1),
            ..CacheSettings::default()
        },
        ..RedisCacheSettings::default()
    };

    assert!(matches!(
        store.clone().with_settings(too_long),
        Err(CacheError::InvalidSettings {
            field: "ttl_secs",
            ..
        })
    ));
    assert!(matches!(
        store.set(&key(1), 1_u32, ttl(Duration::MAX)).await,
        Err(RedisCacheError::Cache(CacheError::InvalidSettings {
            field: "ttl",
            ..
        }))
    ));
    assert_eq!(store.get::<u32>(&key(1)).await.unwrap(), None);
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
async fn get_or_set_stores_over_a_value_that_does_not_decode() {
    let redis = Redis::start().await;
    let store = store(&redis).await;

    store.set(&key(1), 3_u64, None).await.unwrap();

    let loaded = store
        .get_or_set(
            &key(1),
            || async { Ok::<_, ()>(vec!["ada".to_owned()]) },
            None,
        )
        .await;

    assert_eq!(loaded, Ok(vec!["ada".to_owned()]));
    assert_eq!(
        store.get::<Vec<String>>(&key(1)).await.unwrap(),
        Some(vec!["ada".to_owned()])
    );
}

#[tokio::test]
async fn remove_prefix_walks_every_scan_page_and_keeps_other_prefixes() {
    let redis = Redis::start().await;
    // A small batch, so SCAN needs many pages.
    let store = store(&redis)
        .await
        .with_settings(RedisCacheSettings {
            scan_batch_size: 5,
            ..RedisCacheSettings::default()
        })
        .unwrap();
    let username = CacheKey::new("username", 1).unwrap();

    for id in 0..50 {
        store.set(&key(id), id, None).await.unwrap();
    }
    store.set(&username, 1_u32, None).await.unwrap();

    assert_eq!(
        store.remove_prefix("u*").await.unwrap(),
        0,
        "`*` in a prefix is a plain character"
    );
    assert_eq!(store.remove_prefix("user").await.unwrap(), 50);
    assert_eq!(
        store.get::<u32>(&username).await.unwrap(),
        Some(1),
        "`user` does not match `username`"
    );
}

#[tokio::test]
async fn a_versioned_prefix_drops_only_its_version() {
    let redis = Redis::start().await;
    let store = store(&redis).await;
    let v1 = CacheKey::new("user:v1", 1).unwrap();
    let v2 = CacheKey::new("user:v2", 1).unwrap();

    store.set(&v1, 1_u32, None).await.unwrap();
    store.set(&v2, 2_u32, None).await.unwrap();

    assert_eq!(store.remove_prefix("user:v1").await.unwrap(), 1);
    assert_eq!(store.get::<u32>(&v2).await.unwrap(), Some(2));

    store.set(&v1, 1_u32, None).await.unwrap();
    assert_eq!(
        store.remove_prefix("user").await.unwrap(),
        2,
        "the name drops every version"
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
    let redis = Redis::start().await;
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
