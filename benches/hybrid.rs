//! The hybrid store over Redis in Docker on this machine: what a local hit saves, and what a
//! write costs. A write's invalidation is published in the background, so it isn't timed.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use sisa_caching_benches::{User, key, meter_provider, time_after, user};
use sisa_caching_core::cache::CacheStore;
use sisa_caching_hybrid::{
    backplane::RedisBackplane, settings::HybridCacheSettings, store::HybridCacheStore,
};
use sisa_caching_redis::store::RedisCacheStore;
use sisa_caching_testing::{Redis, eventually};
use tokio::runtime::Runtime;

fn hybrid(criterion: &mut Criterion) {
    meter_provider();

    let runtime = Runtime::new().unwrap();
    // Removing the container at the end needs the runtime.
    let _runtime = runtime.enter();
    let redis = runtime.block_on(Redis::start());
    let store = runtime.block_on(async {
        let backplane = RedisBackplane::connect(redis.url()).await.unwrap();
        let store = HybridCacheStore::new(
            RedisCacheStore::new("app", redis.connection().await).unwrap(),
            backplane,
            &HybridCacheSettings::default(),
        )
        .unwrap();

        store.start_worker();
        eventually("the backplane worker subscribes", async || {
            store.local_reads_enabled()
        })
        .await;
        store.set(&key(1), user(1), None).await.unwrap();
        store
    });
    // Built before timing, so the times leave out formatting a key.
    let (hit, miss, written) = (key(1), key(3), key(2));
    let mut group = criterion.benchmark_group("hybrid");

    group.bench_function("get local hit", |bencher| {
        bencher
            .to_async(&runtime)
            .iter(async || store.get::<User>(black_box(&hit)).await.unwrap());
    });
    group.bench_function("get local miss, Redis hit", |bencher| {
        bencher.to_async(&runtime).iter_custom(async |iters| {
            time_after(
                iters,
                async || _ = store.local_store().remove(&hit).await.unwrap(),
                async || _ = black_box(store.get::<User>(black_box(&hit)).await.unwrap()),
            )
            .await
        });
    });
    group.bench_function("get miss", |bencher| {
        bencher
            .to_async(&runtime)
            .iter(async || store.get::<User>(black_box(&miss)).await.unwrap());
    });
    group.bench_function("set", |bencher| {
        bencher
            .to_async(&runtime)
            .iter(async || store.set(black_box(&written), user(2), None).await.unwrap());
    });
    group.bench_function("remove", |bencher| {
        bencher.to_async(&runtime).iter_custom(async |iters| {
            time_after(
                iters,
                async || store.set(&written, user(2), None).await.unwrap(),
                async || assert!(store.remove(black_box(&written)).await.unwrap()),
            )
            .await
        });
    });
    group.bench_function("get_or_set local hit", |bencher| {
        bencher.to_async(&runtime).iter(async || {
            store
                .get_or_set(black_box(&hit), async || Ok::<_, ()>(user(1)), None)
                .await
                .unwrap()
        });
    });
    group.bench_function("get_or_set miss", |bencher| {
        bencher.to_async(&runtime).iter_custom(async |iters| {
            time_after(
                iters,
                async || _ = store.remove(&written).await.unwrap(),
                async || {
                    black_box(
                        store
                            .get_or_set(black_box(&written), async || Ok::<_, ()>(user(2)), None)
                            .await
                            .unwrap(),
                    );
                },
            )
            .await
        });
    });
    group.finish();
}

criterion_group!(benches, hybrid);
criterion_main!(benches);
