//! The users API on the hybrid cache: each instance keeps hot entries in memory, shares them
//! through Redis, and drops its copy when another instance writes.
//!
//! Start Redis (`docker run -p 6379:6379 redis:8-alpine`), then run two instances:
//!
//! ```sh
//! PORT=3000 cargo run -p axum-hybrid
//! PORT=3001 cargo run -p axum-hybrid
//!
//! curl localhost:3001/users/1      # loads, then keeps a local copy
//! curl -X PUT localhost:3000/users/1 -H 'content-type: application/json' -d '{"name":"Ada L."}'
//! curl localhost:3001/users/1      # the new name: 3001 dropped its copy
//! ```
//!
//! `REDIS_URL` picks another server.

use sisa_caching::{
    BackplaneWorker, CacheSettings, HybridCacheSettings, HybridCacheStore, InMemoryCacheSettings,
    RedisCacheSettings, WorkerHost,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let telemetry = users_api::init_telemetry("axum-hybrid")?;
    let url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".into());
    let settings = HybridCacheSettings {
        // Each instance keeps up to 10,000 entries in memory, for at most a minute each. The
        // minute bounds how long an instance can serve a stale entry if an invalidation never
        // reaches it.
        local: InMemoryCacheSettings {
            max_entries: Some(10_000),
            max_ttl_secs: Some(60),
            purge_interval_secs: Some(60),
            ..Default::default()
        },
        // Redis keeps entries ten minutes and reloads busy ones ahead of expiry, as in axum-redis.
        distributed: RedisCacheSettings {
            common: CacheSettings {
                ttl_secs: Some(600),
                refresh_threshold: Some(0.8),
                lock_timeout_ms: Some(5_000),
            },
            ..Default::default()
        },
        ..Default::default()
    };

    // The local layer, the Redis store and the backplane, all on one server.
    let cache = HybridCacheStore::connect("example", url, &settings).await?;

    // The backplane worker applies other instances' invalidations. Until it has subscribed, reads
    // skip the local layer, so serving right away is safe.
    let workers = WorkerHost::new();
    workers.register(BackplaneWorker::new(&cache).with_settings(settings.worker));

    let served = users_api::serve(users_api::router(cache)).await;
    workers.shutdown().await;

    telemetry.shutdown();

    Ok(served?)
}
