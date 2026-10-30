//! The users API on a Redis cache. Every instance shares the same entries, so a write on one is
//! read by the others straight away.
//!
//! Start Redis (`docker run -p 6379:6379 redis:8-alpine`), then:
//!
//! ```sh
//! cargo run -p axum-redis
//!
//! curl localhost:3000/users/1      # loads from the database
//! curl localhost:3000/users/1      # served from Redis
//! redis-cli hgetall example::user:v1:1
//! ```
//!
//! `REDIS_URL` picks another server and `PORT` another port.

use sisa_caching::{RedisCacheSettings, RedisCacheStore};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let telemetry = users_api::init_telemetry("axum-redis")?;
    let url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".into());

    // Entries live ten minutes. Once 80% of that has passed, the next read of a key reloads it
    // while other reads are still served the cached value, so busy keys never expire under load.
    // A loader stuck longer than five seconds stops holding up other requests for its key. The
    // connection reconnects by itself if Redis restarts.
    let mut settings = RedisCacheSettings::default();
    settings.common.ttl_secs = Some(600);
    settings.common.refresh_threshold = Some(0.8);
    settings.common.lock_timeout_ms = Some(5_000);
    let cache = RedisCacheStore::connect("example", url)
        .await?
        .with_settings(settings)?;

    let served = users_api::serve(users_api::router(cache)).await;

    telemetry.shutdown();

    Ok(served?)
}
