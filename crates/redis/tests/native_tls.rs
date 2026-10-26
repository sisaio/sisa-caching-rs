//! The Redis store over redis-rs's native-tls, on a server that only speaks TLS, started in
//! Docker for each test. Only with `tls-native-tls` alone: with `tls-rustls` on too, redis-rs
//! uses rustls.

#![cfg(all(feature = "tls-native-tls", not(feature = "tls-rustls")))]

use sisa_caching_core::cache::{CacheKey, CacheStore};
use sisa_caching_redis::store::RedisCacheStore;
use sisa_caching_testing::RedisTls;

#[tokio::test]
async fn a_store_works_over_native_tls() {
    let tls = RedisTls::start_without_client_certificates().await;
    // The test CA isn't in the system's trust store, so skip the check.
    let url = format!("{}/#insecure", tls.url());
    let store = RedisCacheStore::connect("app", url.as_str()).await.unwrap();
    let key = CacheKey::new("user", 1).unwrap();

    store.set(&key, "ada".to_owned(), None).await.unwrap();

    assert_eq!(
        store.get::<String>(&key).await.unwrap().as_deref(),
        Some("ada")
    );
}

#[tokio::test]
async fn connect_refuses_a_server_the_system_does_not_trust() {
    let tls = RedisTls::start_without_client_certificates().await;

    let error = RedisCacheStore::connect("app", tls.url())
        .await
        .err()
        .unwrap();

    assert!(
        error.to_string().contains("certificate verify failed"),
        "{error}"
    );
}
