//! The Redis store's reads and writes, against a server in Docker on this machine. Most of each
//! time is the round trip to the server.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use sisa_caching_benches::{User, key, user};
use sisa_caching_core::cache::CacheStore;
use sisa_caching_redis::store::RedisCacheStore;
use sisa_caching_testing::Redis;
use tokio::runtime::Runtime;

fn redis(c: &mut Criterion) {
    let runtime = Runtime::new().unwrap();
    // Removing the container at the end needs the runtime.
    let _runtime = runtime.enter();
    let redis = runtime.block_on(Redis::start());
    let store = RedisCacheStore::new("app", runtime.block_on(redis.connection())).unwrap();
    let mut group = c.benchmark_group("redis");

    runtime.block_on(store.set(&key(1), user(1), None)).unwrap();

    group.bench_function("get hit", |b| {
        b.to_async(&runtime)
            .iter(|| async { store.get::<User>(black_box(&key(1))).await.unwrap() });
    });
    group.bench_function("get miss", |b| {
        b.to_async(&runtime)
            .iter(|| async { store.get::<User>(black_box(&key(2))).await.unwrap() });
    });
    group.bench_function("set", |b| {
        b.to_async(&runtime)
            .iter(|| async { store.set(black_box(&key(3)), user(3), None).await.unwrap() });
    });
    group.finish();
}

criterion_group!(benches, redis);
criterion_main!(benches);
