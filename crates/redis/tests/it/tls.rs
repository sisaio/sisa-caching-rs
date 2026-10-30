//! The Redis store on a server that only speaks TLS, started in Docker for each test. Only with
//! `tls-rustls`, whose crypto provider these tests check is there.

#![cfg(feature = "tls-rustls")]

use sisa_caching_core::cache::{CacheKey, CacheStore};
use sisa_caching_redis::store::RedisCacheStore;
use sisa_caching_testing::RedisTls;

#[tokio::test]
async fn a_store_works_over_tls_with_a_private_ca_and_a_client_certificate() {
    let tls = RedisTls::start().await;
    let store = RedisCacheStore::from_client("app", tls.client())
        .await
        .unwrap();
    let key = CacheKey::new("user", 1).unwrap();

    store.set(&key, "ada".to_owned(), None).await.unwrap();

    assert_eq!(
        store.get::<String>(&key).await.unwrap().as_deref(),
        Some("ada")
    );
}

#[tokio::test]
async fn connect_refuses_a_server_the_system_roots_do_not_trust() {
    let tls = RedisTls::start().await;

    let error = RedisCacheStore::connect("app", tls.url())
        .await
        .err()
        .unwrap();

    assert!(error.to_string().contains("UnknownIssuer"), "{error}");
}
