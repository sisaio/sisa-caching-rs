//! The users API on the store its configuration picks, here the `CACHE` variable: `memory` (the
//! default), `redis` or `hybrid`. `REDIS_URL` picks another server and `PORT` another port.
//!
//! ```sh
//! cargo run -p axum-config
//!
//! curl localhost:3000/users/1      # loads from the database
//! curl localhost:3000/users/1      # served from the cache
//! curl localhost:3000/users        # the list
//! curl localhost:3000/users/count  # the count
//! curl -X PUT localhost:3000/users/4 -H 'content-type: application/json' -d '{"name":"Barbara"}'
//! curl localhost:3000/users/count  # reloaded: 4
//! ```
//!
//! The Redis and hybrid stores need a server (`docker run -p 6379:6379 redis:8-alpine`). With
//! Redis, every instance shares the same entries. With hybrid, each instance also keeps hot
//! entries in memory and drops its copy a moment after another instance writes. Run each instance
//! in a terminal of its own, and the requests in a third:
//!
//! ```sh
//! CACHE=hybrid PORT=3000 cargo run -p axum-config
//! CACHE=hybrid PORT=3001 cargo run -p axum-config
//!
//! curl localhost:3001/users/1      # loads, then keeps a local copy
//! curl -X PUT localhost:3000/users/1 -H 'content-type: application/json' -d '{"name":"Ada L."}'
//! curl localhost:3001/users/1      # the new name once 3001 has dropped its copy
//! redis-cli hgetall example::user:v1:1
//! ```
//!
//! `CacheStore` has methods generic over the value type, so there is no `dyn CacheStore` to hold
//! whichever store the configuration names. Pick it where code generic over the store meets
//! something that isn't, as here, where each arm builds an axum `Router`. Code that keeps the
//! store itself, such as in a struct field, needs an enum over the three stores instead.

use sisa_caching::{
    HybridCacheSettings, HybridCacheStore, InMemoryCacheSettings, InMemoryCacheStore,
    RedisCacheSettings, RedisCacheStore,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let telemetry = users_api::init_telemetry("axum-config")?;
    let url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".into());

    // A hybrid store is kept, to shut it down once serving ends.
    let mut hybrid = None;
    let app = match std::env::var("CACHE").as_deref().unwrap_or("memory") {
        "memory" => {
            // Up to 10,000 entries, each for five minutes unless a write says otherwise. The store
            // sweeps out expired entries every minute, from its first write on. A loader stuck
            // longer than five seconds stops holding up other requests for its key.
            let mut settings = InMemoryCacheSettings::default();
            settings.common.ttl_secs = Some(300);
            settings.common.lock_timeout_ms = Some(5_000);
            settings.max_entries = Some(10_000);
            settings.purge_interval_secs = Some(60);

            users_api::router(InMemoryCacheStore::new("example")?.with_settings(settings)?)
        }
        "redis" => {
            // Entries live ten minutes. Once 80% of that has passed, the next read of a key
            // reloads it while other reads are still served the cached value, so busy keys never
            // expire under load. The connection reconnects by itself if Redis restarts.
            let mut settings = RedisCacheSettings::default();
            settings.common.ttl_secs = Some(600);
            settings.common.refresh_threshold = Some(0.8);
            settings.common.lock_timeout_ms = Some(5_000);

            users_api::router(
                RedisCacheStore::connect("example", url)
                    .await?
                    .with_settings(settings)?,
            )
        }
        "hybrid" => {
            // Each instance keeps up to 10,000 entries in memory, for at most a minute each, which
            // caps how long it can serve a stale entry when a lost invalidation goes unnoticed.
            // Redis keeps entries as the `redis` arm does.
            let mut settings = HybridCacheSettings::default();
            settings.local.max_entries = Some(10_000);
            settings.local.max_ttl_secs = Some(60);
            settings.distributed.common.ttl_secs = Some(600);
            settings.distributed.common.refresh_threshold = Some(0.8);
            settings.distributed.common.lock_timeout_ms = Some(5_000);

            // The local layer, the Redis store, the backplane, and the worker that applies other
            // instances' invalidations. Until it has subscribed, reads skip the local layer, so
            // serving right away is safe.
            let cache = HybridCacheStore::connect("example", url, &settings).await?;

            hybrid = Some(cache.clone());
            users_api::router(cache)
        }
        other => return Err(format!("CACHE is memory, redis or hybrid, not {other}").into()),
    };

    let served = users_api::serve(app).await;

    // Tells the other instances this one is gone, so they keep their local copies.
    if let Some(cache) = hybrid {
        cache.shutdown().await;
    }
    telemetry.shutdown();

    Ok(served?)
}
