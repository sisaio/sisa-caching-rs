//! What the benchmarks store: a small user record under `user:{id}`, the meter provider they
//! record their metrics with, and a timer for calls that need the store set up before each one.

use std::{
    sync::OnceLock,
    time::{Duration, Instant},
};

use opentelemetry::global;
use opentelemetry_sdk::metrics::{ManualReader, SdkMeterProvider};
use serde::{Deserialize, Serialize};
use sisa_caching_core::cache::CacheKey;

#[derive(Clone, Serialize, Deserialize)]
pub struct User {
    pub id: u64,
    pub name: String,
    pub email: String,
}

pub fn user(id: u64) -> User {
    User {
        id,
        name: "Ada Lovelace".to_owned(),
        email: "ada@example.com".to_owned(),
    }
}

pub fn key(id: u64) -> CacheKey {
    CacheKey::new("user", id).unwrap()
}

/// Installs a meter provider, once, so the stores record their metrics as an application that
/// exports them would. Its reader is never collected, so nothing is exported. Call it before
/// building the first store: the stores' instruments come from the provider installed then.
pub fn meter_provider() {
    static PROVIDER: OnceLock<SdkMeterProvider> = OnceLock::new();

    PROVIDER.get_or_init(|| {
        let provider = SdkMeterProvider::builder()
            .with_reader(ManualReader::builder().build())
            .build();

        global::set_meter_provider(provider.clone());
        provider
    });
}

/// Runs `prepare` then `call`, `iters` times, and returns the time `call` took in all: for a call
/// that needs the store in some state first, such as a key to remove or a key that isn't there.
pub async fn time_after(
    iters: u64,
    mut prepare: impl AsyncFnMut(),
    mut call: impl AsyncFnMut(),
) -> Duration {
    let mut total = Duration::ZERO;

    for _ in 0..iters {
        prepare().await;

        let start = Instant::now();

        call().await;
        total += start.elapsed();
    }
    total
}
