//! Hybrid nodes over redis-rs's native-tls, on a server that only speaks TLS. Each test starts
//! its own server in Docker. Only with `tls-native-tls` alone: with `tls-rustls` on too, redis-rs
//! uses rustls.

#![cfg(all(feature = "tls-native-tls", not(feature = "tls-rustls")))]

mod common;

use common::{assert_a_write_reaches, start};
use sisa_caching_hybrid::store::RedisHybridCacheStore;
use sisa_caching_testing::RedisTls;

async fn node(tls: &RedisTls) -> RedisHybridCacheStore {
    // The test CA isn't in the system's trust store, so skip the check.
    let url = format!("{}/#insecure", tls.url());

    start(
        RedisHybridCacheStore::connect("users", url.as_str(), &Default::default())
            .await
            .unwrap(),
    )
    .await
}

#[tokio::test]
async fn a_write_on_one_node_evicts_the_copy_on_another_over_native_tls() {
    let tls = RedisTls::start_without_client_certificates().await;

    assert_a_write_reaches(&node(&tls).await, &node(&tls).await).await;
}
