//! What a Redis store counts and logs while its server is down. A test binary of its own: the
//! instruments are created when the first store is built, from the meter provider installed then,
//! so this test must install its provider before any other test builds a store.

use opentelemetry::KeyValue;
use sisa_caching_core::cache::{CacheKey, CacheStore};
use sisa_caching_redis::store::RedisCacheStore;
use sisa_caching_testing::{Logs, Metrics, Redis, eventually};

fn key(id: u64) -> CacheKey {
    CacheKey::new("user", id).unwrap()
}

#[tokio::test]
async fn an_outage_logs_once_per_operation_and_counts_every_failure() {
    let metrics = Metrics::install();
    let logs = Logs::default();
    let _logging = tracing::subscriber::set_default(logs.clone());
    let redis = Redis::start_restartable().await;
    let store = RedisCacheStore::connect("app", redis.url()).await.unwrap();
    let load = || async { Ok::<_, ()>("loaded".to_owned()) };

    // Data that doesn't decode as the requested type is a miss: the caller gets no value.
    store.set(&key(0), "ada".to_owned(), None).await.unwrap();
    assert!(store.get::<u64>(&key(0)).await.is_err());
    assert_eq!(
        metrics.value("sisa.caching.misses", &[KeyValue::new("layer", "redis")]),
        1
    );

    redis.stop().await;

    for id in 1..=10 {
        let loaded = store.get_or_set(&key(id), load, None).await;

        assert_eq!(loaded.as_deref(), Ok("loaded"));
    }

    // Each call read twice, before and after its lock, and stored once.
    assert_eq!(
        metrics.value("sisa.caching.errors", &[KeyValue::new("operation", "get")]),
        20
    );
    assert_eq!(
        metrics.value("sisa.caching.errors", &[KeyValue::new("operation", "set")]),
        10
    );

    redis.restart().await;
    eventually("the store reconnects", || async {
        store.get::<String>(&key(1)).await.is_ok()
    })
    .await;
    // Both reads work, then storing what it loaded does.
    store.get_or_set(&key(1), load, None).await.unwrap();

    let lines: Vec<String> = logs
        .lines()
        .into_iter()
        .filter(|line| line.contains("operation="))
        .collect();
    let logged = |level: &str, operation: &str| {
        lines
            .iter()
            .filter(|line| {
                line.starts_with(level) && line.contains(&format!(r#"operation="{operation}""#))
            })
            .count()
    };

    assert_eq!(lines.len(), 4, "{lines:#?}");
    assert_eq!(logged("WARN", "get"), 1, "{lines:#?}");
    assert_eq!(logged("WARN", "set"), 1, "{lines:#?}");
    assert_eq!(logged("INFO", "get"), 1, "{lines:#?}");
    assert_eq!(logged("INFO", "set"), 1, "{lines:#?}");
}
