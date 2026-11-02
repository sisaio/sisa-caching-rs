//! The Redis store's reads and writes, against a server in Docker on this machine. Most of each
//! time is the round trip to the server.

use criterion::{Criterion, criterion_group, criterion_main};
use sisa_caching_benches::{meter_provider, time_every_call};
use sisa_caching_redis::store::RedisCacheStore;
use sisa_caching_testing::Redis;
use tokio::runtime::Runtime;

fn redis(criterion: &mut Criterion) {
    meter_provider();

    let runtime = Runtime::new().unwrap();
    // Removing the container at the end needs the runtime.
    let _runtime = runtime.enter();
    let redis = runtime.block_on(Redis::start());
    let store = RedisCacheStore::new("app", runtime.block_on(redis.connection())).unwrap();
    let mut group = criterion.benchmark_group("redis");

    time_every_call(&mut group, &runtime, &store, "hit");
    group.finish();
}

criterion_group! {
    name = benches;
    config = sisa_caching_benches::criterion();
    targets = redis
}
criterion_main!(benches);
