//! What the benchmarks store: a small user record under `user:{id}`, the meter provider they
//! record their metrics with, a timer for calls that need the store set up before each one, and
//! the calls every store has, timed the same way on each.

use std::{
    hint::black_box,
    sync::OnceLock,
    time::{Duration, Instant},
};

use criterion::{BenchmarkGroup, measurement::WallTime};
use opentelemetry::global;
use opentelemetry_sdk::metrics::{ManualReader, SdkMeterProvider};
use serde::{Deserialize, Serialize};
use sisa_caching_core::cache::{CacheKey, CacheStore};
use tokio::runtime::Runtime;

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

/// Times the calls every store has, in `group`: `get` hit and miss, `set`, `remove`, and
/// `get_or_set` hit and miss. `hit` names the hits, such as `local hit` on the hybrid. It caches
/// `user:1` first; `user:2` stays missing, and `user:3` is the key the writes use.
pub fn time_every_call<S: CacheStore>(
    group: &mut BenchmarkGroup<'_, WallTime>,
    runtime: &Runtime,
    store: &S,
    hit: &str,
) {
    // Built before timing, so the times leave out formatting a key.
    let (cached, missing, written) = (key(1), key(2), key(3));

    runtime.block_on(store.set(&cached, user(1), None)).unwrap();

    group.bench_function(format!("get {hit}"), |bencher| {
        bencher
            .to_async(runtime)
            .iter(async || store.get::<User>(black_box(&cached)).await.unwrap());
    });
    group.bench_function("get miss", |bencher| {
        bencher
            .to_async(runtime)
            .iter(async || store.get::<User>(black_box(&missing)).await.unwrap());
    });
    group.bench_function("set", |bencher| {
        bencher
            .to_async(runtime)
            .iter(async || store.set(black_box(&written), user(3), None).await.unwrap());
    });
    group.bench_function("remove", |bencher| {
        bencher.to_async(runtime).iter_custom(async |iters| {
            time_after(
                iters,
                async || store.set(&written, user(3), None).await.unwrap(),
                async || assert!(store.remove(black_box(&written)).await.unwrap()),
            )
            .await
        });
    });
    group.bench_function(format!("get_or_set {hit}"), |bencher| {
        bencher.to_async(runtime).iter(async || {
            store
                .get_or_set(black_box(&cached), async || Ok::<_, ()>(user(1)), None)
                .await
                .unwrap()
        });
    });
    group.bench_function("get_or_set miss", |bencher| {
        bencher.to_async(runtime).iter_custom(async |iters| {
            time_after(
                iters,
                async || _ = store.remove(&written).await.unwrap(),
                async || {
                    black_box(
                        store
                            .get_or_set(black_box(&written), async || Ok::<_, ()>(user(3)), None)
                            .await
                            .unwrap(),
                    );
                },
            )
            .await
        });
    });
}
