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

use sisa_caching::{HybridCacheSettings, HybridCacheStore};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let telemetry = users_api::init_telemetry("axum-hybrid")?;
    let url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".into());
    let mut settings = HybridCacheSettings::default();
    // Each instance keeps up to 10,000 entries in memory, for at most a minute each, which caps
    // how long it can serve a stale entry when a lost invalidation goes unnoticed.
    settings.local.max_entries = Some(10_000);
    settings.local.max_ttl_secs = Some(60);
    settings.local.purge_interval_secs = Some(60);
    // Redis keeps entries ten minutes and reloads busy ones ahead of expiry, as in axum-redis.
    settings.distributed.common.ttl_secs = Some(600);
    settings.distributed.common.refresh_threshold = Some(0.8);
    settings.distributed.common.lock_timeout_ms = Some(5_000);

    // The local layer, the Redis store and the backplane, all on one server, and the worker that
    // applies other instances' invalidations. Until it has subscribed, reads skip the local layer,
    // so serving right away is safe.
    let cache = HybridCacheStore::connect("example", url, &settings).await?;

    let served = users_api::serve(users_api::router(cache.clone())).await;

    // Tells the other instances this one is gone, so they keep their local copies.
    cache.shutdown().await;
    telemetry.shutdown();

    Ok(served?)
}
