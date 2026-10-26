//! The users API on an in-memory cache: one process, no server needed.
//!
//! ```sh
//! cargo run -p axum-memory
//!
//! curl localhost:3000/users/1      # loads from the database
//! curl localhost:3000/users/1      # served from the cache
//! curl localhost:3000/users        # the list
//! curl localhost:3000/users/count  # the count
//! curl -X PUT localhost:3000/users/4 -H 'content-type: application/json' -d '{"name":"Barbara"}'
//! curl localhost:3000/users/count  # reloaded: 4
//! ```

use sisa_caching::{
    CacheSettings, InMemoryCacheSettings, InMemoryCacheStore, PurgeWorker, WorkerHost,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let telemetry = users_api::init_telemetry("axum-memory")?;
    // Up to 10,000 entries, each for five minutes unless a write says otherwise. Expired entries
    // are swept every minute. A loader stuck longer than five seconds stops holding up other
    // requests for its key.
    let cache = InMemoryCacheStore::new("example")?.with_settings(InMemoryCacheSettings {
        common: CacheSettings {
            ttl_secs: Some(300),
            lock_timeout_ms: Some(5_000),
            ..Default::default()
        },
        max_entries: Some(10_000),
        purge_interval_secs: Some(60),
        ..Default::default()
    })?;

    let workers = WorkerHost::new();
    workers.register(PurgeWorker::new(cache.clone()));

    let served = users_api::serve(users_api::router(cache)).await;
    workers.shutdown().await;

    telemetry.shutdown();

    Ok(served?)
}
