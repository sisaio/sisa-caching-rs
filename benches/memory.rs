//! The in-memory store's reads and writes, alone and from many tasks at once, and the calls that
//! walk every entry of a large store.

use std::{
    hint::black_box,
    ops::Range,
    time::{Duration, Instant},
};

use criterion::{BenchmarkId, Criterion, SamplingMode, criterion_group, criterion_main};
use futures_util::FutureExt;
use sisa_caching_benches::{User, key, meter_provider, user};
use sisa_caching_core::cache::{CacheOptions, CacheStore};
use sisa_caching_memory::{settings::InMemoryCacheSettings, store::InMemoryCacheStore};
use sisa_caching_testing::ManualClock;
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

/// Writes a user under each id, expiring after `ttl` if one is given.
fn fill(runtime: &Runtime, store: &InMemoryCacheStore, ids: Range<u64>, ttl: Option<Duration>) {
    let options = ttl.map(CacheOptions::with_ttl);

    runtime.block_on(async {
        for id in ids {
            store
                .set(&key(id), user(id), options.clone())
                .await
                .unwrap();
        }
    });
}

fn single(c: &mut Criterion) {
    meter_provider();

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
    meter_provider();

    const TASKS: u64 = 8;

    let runtime = Runtime::new().unwrap();
    let mut group = c.benchmark_group("memory 8 tasks");

    for shards in [1, 16] {
        let store = store(shards);

        fill(&runtime, &store, 0..TASKS, None);

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

/// The calls that walk every entry of one map, at a million entries. Each holds that map's lock,
/// and the thread it runs on, until it is done.
fn large(c: &mut Criterion) {
    meter_provider();

    const ENTRIES: u64 = 1_000_000;

    let runtime = Runtime::new().unwrap();
    let mut group = c.benchmark_group("memory 1M entries");

    // Each call takes milliseconds, and some need a full store rebuilt before them.
    group
        .sample_size(10)
        .sampling_mode(SamplingMode::Flat)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3));

    let filled = store(1);

    fill(&runtime, &filled, 0..ENTRIES, None);
    group.bench_function("remove_prefix, nothing matches", |b| {
        b.to_async(&runtime)
            .iter(|| async { filled.remove_prefix(black_box("other")).await.unwrap() });
    });
    group.bench_function("purge, nothing expired", |b| b.iter(|| filled.purge()));
    drop(filled);

    group.bench_function("purge, every entry expired", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;

            for _ in 0..iters {
                let clock = ManualClock::new();
                let expired = store(1).with_clock(clock.clone());

                fill(&runtime, &expired, 0..ENTRIES, Some(Duration::from_secs(1)));
                clock.advance(Duration::from_secs(2));

                let start = Instant::now();

                black_box(expired.purge());
                total += start.elapsed();
            }
            total
        });
    });

    // A trim leaves nine tenths of the cap, so each timed write first tops the store up to it. The
    // write runs outside the runtime, where it runs its trim itself; inside one, the trim runs on
    // the blocking pool and the write doesn't wait for it.
    let full = InMemoryCacheStore::new("app")
        .unwrap()
        .with_settings(InMemoryCacheSettings {
            max_entries: Some(ENTRIES as usize),
            ..InMemoryCacheSettings::default()
        })
        .unwrap();
    let mut next = ENTRIES - ENTRIES / 10;

    fill(&runtime, &full, 0..next, None);
    group.bench_function("set that trims", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;

            for _ in 0..iters {
                fill(&runtime, &full, next..next + ENTRIES / 10, None);
                next += ENTRIES / 10;

                let (over, value) = (key(next), user(next));
                let start = Instant::now();

                full.set(&over, value, None)
                    .now_or_never()
                    .unwrap()
                    .unwrap();
                total += start.elapsed();
                next += 1;
            }
            total
        });
    });
    group.finish();
}

criterion_group!(benches, single, parallel, large);
criterion_main!(benches);
