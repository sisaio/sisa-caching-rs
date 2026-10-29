//! The Redis store's reads and writes, against a server in Docker on this machine. Most of each
//! time is the round trip to the server.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use sisa_caching_benches::{User, key, meter_provider, user};
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
            .iter(|| async { store.get::<User>(black_box(&hit)).await.unwrap() });
    });
    group.bench_function("get miss", |b| {
        b.to_async(&runtime)
            .iter(|| async { store.get::<User>(black_box(&miss)).await.unwrap() });
    });
    group.bench_function("set", |b| {
        b.to_async(&runtime)
            .iter(|| async { store.set(black_box(&written), user(3), None).await.unwrap() });
    });
    group.finish();
}

criterion_group!(benches, redis);
criterion_main!(benches);
