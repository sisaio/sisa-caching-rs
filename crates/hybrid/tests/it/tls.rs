//! Hybrid nodes on a server that only speaks TLS, with a private CA and a client certificate.
//! Each test starts its own server in Docker. Only with `tls-rustls`, whose crypto provider these
//! tests check is there.

#![cfg(feature = "tls-rustls")]

use crate::common::{assert_a_write_reaches, start};
use sisa_caching_hybrid::{
    backplane::RedisBackplane,
    store::{HybridCacheStore, RedisHybridCacheStore},
};
use sisa_caching_memory::store::InMemoryCacheStore;
use sisa_caching_redis::store::RedisCacheStore;
use sisa_caching_testing::RedisTls;

async fn node(tls: &RedisTls) -> RedisHybridCacheStore {
    let backplane = RedisBackplane::from_client(tls.client(), Default::default())
        .await
        .unwrap();

    start(
        HybridCacheStore::new(
            InMemoryCacheStore::new("users").unwrap(),
            RedisCacheStore::from_client("users", tls.client())
                .await
                .unwrap(),
            backplane,
        )
        .unwrap(),
    )
    .await
}

#[tokio::test]
async fn a_write_on_one_node_evicts_the_copy_on_another_over_tls() {
    let tls = RedisTls::start().await;

    assert_a_write_reaches(&node(&tls).await, &node(&tls).await).await;
}
