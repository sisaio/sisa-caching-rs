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

use sisa_caching::{InMemoryCacheSettings, InMemoryCacheStore};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let telemetry = users_api::init_telemetry("axum-memory")?;
    // Up to 10,000 entries, each for five minutes unless a write says otherwise. The store sweeps
    // out expired entries every minute, from its first write on. A loader stuck longer than five
    // seconds stops holding up other requests for its key.
    let mut settings = InMemoryCacheSettings::default();
    settings.common.ttl_secs = Some(300);
    settings.common.lock_timeout_ms = Some(5_000);
    settings.max_entries = Some(10_000);
    settings.purge_interval_secs = Some(60);
    let cache = InMemoryCacheStore::new("example")?.with_settings(settings)?;

    let served = users_api::serve(users_api::router(cache)).await;

    telemetry.shutdown();

    Ok(served?)
}
