//! The in-memory store's reads and writes, alone and from many tasks at once.

use std::{hint::black_box, time::Instant};

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use sisa_caching_benches::{User, key, user};
use sisa_caching_core::cache::CacheStore;
use sisa_caching_memory::{settings::InMemoryCacheSettings, store::InMemoryCacheStore};
use tokio::runtime::Runtime;

fn store(shards: usize) -> InMemoryCacheStore {
    InMemoryCacheStore::new("app")
        .unwrap()
        .with_settings(InMemoryCacheSettings {
            shards: Some(shards),
            ..InMemoryCacheSettings::default()
        })
        .unwrap()
}

fn single(c: &mut Criterion) {
    let runtime = Runtime::new().unwrap();
    let store = store(1);
    // Built before timing, so the times leave out formatting a key.
    let (hit, miss, written) = (key(1), key(2), key(3));
    let mut group = c.benchmark_group("memory");

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
    group.bench_function("get_or_set hit", |b| {
        b.to_async(&runtime).iter(|| async {
            store
                .get_or_set(black_box(&hit), || async { Ok::<_, ()>(user(1)) }, None)
                .await
                .unwrap()
        });
    });
    group.finish();
}

/// Eight tasks reading different keys, through one map or sixteen.
fn parallel(c: &mut Criterion) {
    const TASKS: u64 = 8;

    let runtime = Runtime::new().unwrap();
    let mut group = c.benchmark_group("memory 8 tasks");

    for shards in [1, 16] {
        let store = store(shards);

        for id in 0..TASKS {
            runtime
                .block_on(store.set(&key(id), user(id), None))
                .unwrap();
        }

        group.bench_function(BenchmarkId::new("get hit, shards", shards), |b| {
            b.to_async(&runtime).iter_custom(|iters| {
                let store = store.clone();

                async move {
                    let keys: Vec<_> = (0..TASKS).map(key).collect();
                    let start = Instant::now();
                    let tasks: Vec<_> = keys
                        .into_iter()
                        .map(|key| {
                            let store = store.clone();

                            tokio::spawn(async move {
                                for _ in 0..iters {
                                    black_box(store.get::<User>(&key).await.unwrap());
                                }
                            })
                        })
                        .collect();

                    for task in tasks {
                        task.await.unwrap();
                    }
                    // Per read across all tasks, so more reads in parallel shows as less time.
                    start.elapsed() / TASKS as u32
                }
            });
        });
    }
    group.finish();
}

criterion_group!(benches, single, parallel);
criterion_main!(benches);
