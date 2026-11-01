//! The Redis store's reads and writes, against a server in Docker on this machine. Most of each
//! time is the round trip to the server.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use sisa_caching_benches::{User, key, meter_provider, time_after, user};
use sisa_caching_core::cache::CacheStore;
use sisa_caching_redis::store::RedisCacheStore;
use sisa_caching_testing::Redis;
use tokio::runtime::Runtime;

fn redis(c: &mut Criterion) {
    meter_provider();

    let runtime = Runtime::new().unwrap();
    // Removing the container at the end needs the runtime.
    let _runtime = runtime.enter();
    let redis = runtime.block_on(Redis::start());
    let store = RedisCacheStore::new("app", runtime.block_on(redis.connection())).unwrap();
    // Built before timing, so the times leave out formatting a key.
    let (hit, miss, written) = (key(1), key(2), key(3));
    let mut group = c.benchmark_group("redis");

    runtime.block_on(store.set(&hit, user(1), None)).unwrap();

    group.bench_function("get hit", |b| {
        b.to_async(&runtime)
            .iter(async || store.get::<User>(black_box(&hit)).await.unwrap());
    });
    group.bench_function("get miss", |b| {
        b.to_async(&runtime)
            .iter(async || store.get::<User>(black_box(&miss)).await.unwrap());
    });
    group.bench_function("set", |b| {
        b.to_async(&runtime)
            .iter(async || store.set(black_box(&written), user(3), None).await.unwrap());
    });
    group.bench_function("remove", |b| {
        b.to_async(&runtime).iter_custom(async |iters| {
            time_after(
                iters,
                async || store.set(&written, user(3), None).await.unwrap(),
                async || assert!(store.remove(black_box(&written)).await.unwrap()),
            )
            .await
        });
    });
    group.bench_function("get_or_set hit", |b| {
        b.to_async(&runtime).iter(async || {
            store
                .get_or_set(black_box(&hit), async || Ok::<_, ()>(user(1)), None)
                .await
                .unwrap()
        });
    });
    group.bench_function("get_or_set miss", |b| {
        b.to_async(&runtime).iter_custom(async |iters| {
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
    group.finish();
}

criterion_group!(benches, redis);
criterion_main!(benches);
