//! Test helpers for the Sisa Caching crates. Not published.
//!
//! - [`ManualClock`]: a clock that moves only when the test moves it.
//! - [`Redis`]: a fresh Redis server in Docker for one test, removed when the test ends.
//! - [`RedisCluster`] and [`RedisSentinel`]: the same for a three-primary cluster and for a
//!   primary and replica behind Sentinel.
//! - [`RedisTls`]: a server that only speaks TLS, with or without asking for a client certificate.
//! - [`Proxy`]: a TCP proxy in front of a server that a test can cut, as a network split would.
//! - [`eventually`]: waits for something another task does.
//! - [`Logs`]: the info and higher events logged on the test's thread.
//!
//! `REDIS_IMAGE` picks the server for all of them: Redis, Valkey, Dragonfly or Garnet.

mod clock;
mod cluster;
mod logs;
mod proxy;
mod sentinel;
mod server;
mod standalone;
mod tls;

use std::time::Duration;

pub use crate::{
    clock::ManualClock, cluster::RedisCluster, logs::Logs, proxy::Proxy, sentinel::RedisSentinel,
    server::Server, standalone::Redis, tls::RedisTls,
};

/// Polls `check` until it holds, and fails the test with `what` after five seconds.
pub async fn eventually<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);

    while !check().await {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");

        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
