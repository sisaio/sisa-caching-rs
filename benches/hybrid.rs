//! The hybrid store over Redis in Docker on this machine: what a local hit saves, and what a
//! write costs. A write's invalidation is published in the background, so it isn't timed.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use sisa_caching_benches::{User, key, meter_provider, time_after, time_every_call};
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
        store
    });
    let mut group = criterion.benchmark_group("hybrid");

    time_every_call(&mut group, &runtime, &store, "local hit");

    // Built before timing, so the time leaves out formatting the key `time_every_call` cached.
    let hit = key(1);

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
    group.finish();
}

criterion_group! {
    name = benches;
    config = sisa_caching_benches::criterion();
    targets = hybrid
}
criterion_main!(benches);
