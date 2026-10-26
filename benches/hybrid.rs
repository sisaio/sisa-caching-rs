//! The hybrid store over Redis in Docker on this machine: what a local hit saves, and what a
//! write costs. A write's invalidation is published in the background, so it isn't timed.

use std::{
    hint::black_box,
    time::{Duration, Instant},
};

use criterion::{Criterion, criterion_group, criterion_main};
use sisa_caching_benches::{User, key, user};
use sisa_caching_core::{cache::CacheStore, worker::WorkerHost};
use sisa_caching_hybrid::{
    backplane::RedisBackplane, store::HybridCacheStore, worker::BackplaneWorker,
};
use sisa_caching_memory::store::InMemoryCacheStore;
use sisa_caching_redis::store::RedisCacheStore;
use sisa_caching_testing::{Redis, eventually};
use tokio::runtime::Runtime;

fn hybrid(c: &mut Criterion) {
    let runtime = Runtime::new().unwrap();
    // Removing the container at the end needs the runtime.
    let _runtime = runtime.enter();
    let redis = runtime.block_on(Redis::start());
    let workers = WorkerHost::new();
    let store = runtime.block_on(async {
        let backplane = RedisBackplane::connect(redis.url(), Default::default())
            .await
            .unwrap();
        let store = HybridCacheStore::new(
            InMemoryCacheStore::new("app").unwrap(),
            RedisCacheStore::new("app", redis.connection().await).unwrap(),
            backplane,
        )
        .unwrap();

        workers.register(BackplaneWorker::new(&store));
        eventually("the backplane worker subscribes", || async {
            store.local_reads_enabled()
        })
        .await;
        store.set(&key(1), user(1), None).await.unwrap();
        store
    });
    let mut group = c.benchmark_group("hybrid");

    group.bench_function("get local hit", |b| {
        b.to_async(&runtime)
            .iter(|| async { store.get::<User>(black_box(&key(1))).await.unwrap() });
    });
    group.bench_function("get local miss, Redis hit", |b| {
        b.to_async(&runtime).iter_custom(|iters| {
            let store = &store;

            async move {
                let mut total = Duration::ZERO;

                for _ in 0..iters {
                    store.local_store().remove(&key(1)).await.unwrap();

                    let start = Instant::now();

                    black_box(store.get::<User>(&key(1)).await.unwrap());
                    total += start.elapsed();
                }
                total
            }
        });
    });
    group.bench_function("set", |b| {
        b.to_async(&runtime)
            .iter(|| async { store.set(black_box(&key(2)), user(2), None).await.unwrap() });
    });
    group.finish();
}

criterion_group!(benches, hybrid);
criterion_main!(benches);
