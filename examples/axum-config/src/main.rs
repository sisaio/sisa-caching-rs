//! The users API on the store its configuration picks, here the `CACHE` variable: `memory` (the
//! default), `redis` or `hybrid`.
//!
//! ```sh
//! cargo run -p axum-config
//! CACHE=redis cargo run -p axum-config     # needs Redis, as axum-redis does
//! CACHE=hybrid cargo run -p axum-config
//! ```
//!
//! `CacheStore` has methods generic over the value type, so there is no `dyn CacheStore` to hold
//! whichever store the configuration names. Pick it where code generic over the store meets
//! something that isn't, as here, where each arm builds an axum `Router`. Code that keeps the
//! store itself, such as in a struct field, needs an enum over the three stores instead.

use sisa_caching::{
    CacheSettings, HybridCacheSettings, HybridCacheStore, InMemoryCacheSettings,
    InMemoryCacheStore, RedisCacheSettings, RedisCacheStore,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let telemetry = users_api::init_telemetry("axum-config")?;
    let url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".into());
    // Five minutes everywhere; each store's other settings keep their defaults.
    let mut common = CacheSettings::default();
    common.ttl_secs = Some(300);

    // A hybrid store is kept, to shut it down once serving ends.
    let mut hybrid = None;
    let app = match std::env::var("CACHE").as_deref().unwrap_or("memory") {
        "memory" => {
            let mut settings = InMemoryCacheSettings::default();
            settings.common = common;

            users_api::router(InMemoryCacheStore::new("example")?.with_settings(settings)?)
        }
        "redis" => {
            let mut settings = RedisCacheSettings::default();
            settings.common = common;

            users_api::router(
                RedisCacheStore::connect("example", url)
                    .await?
                    .with_settings(settings)?,
            )
        }
        "hybrid" => {
            let mut settings = HybridCacheSettings::default();
            settings.distributed.common = common;

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
