//! A chaos test: several hybrid nodes, each on a runtime of its own as in a process of its own,
//! write and read shared keys for minutes while the server restarts and freezes, a node is cut off,
//! a node crashes and a new one takes its place, and subscriptions drop. A checker fails the test
//! when any read returns a value that was replaced longer ago than the docs allow.
//!
//! Ignored by `cargo test`. Run it with
//! `cargo test -p sisa-caching-hybrid --test chaos -- --ignored --nocapture`; `CHAOS_SECS` sets how
//! long the faults go on (90 s by default).

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use opentelemetry::global;
use opentelemetry_sdk::metrics::{
    InMemoryMetricExporter, PeriodicReader, SdkMeterProvider,
    data::{AggregatedMetrics, MetricData},
};
use sisa_caching_core::cache::CacheStore;
use sisa_caching_hybrid::{settings::HybridCacheSettings, store::RedisHybridCacheStore};
use sisa_caching_testing::{Proxy, Redis};
use tokio::runtime::Runtime;

/// The nodes, each writing the keys of its own slot and reading every slot's.
const NODES: usize = 4;
/// Keys per slot.
const KEYS: usize = 20;
/// How long a replaced value may still be read with the default settings: a node notices a
/// split within `heartbeat_timeout_ms` plus one `heartbeat_interval_ms` (6 s), and a peer that
/// stopped within 6 s too. The rest is slack for a busy runner.
const BOUND: Duration = Duration::from_secs(10);
/// The time between the start of one fault and the next.
const PERIOD: Duration = Duration::from_secs(15);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "runs for minutes; the nightly chaos job runs it"]
async fn nodes_stay_coherent_through_faults() {
    let exporter = InMemoryMetricExporter::default();
    let provider = SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(exporter.clone()).build())
        .build();

    global::set_meter_provider(provider.clone());

    let chaos = Duration::from_secs(
        std::env::var("CHAOS_SECS")
            .ok()
            .and_then(|secs| secs.parse().ok())
            .unwrap_or(90),
    );
    let redis = Redis::start_restartable().await;
    let history = Arc::new(History::default());
    let mut nodes = Vec::new();

    for slot in 0..NODES {
        nodes.push(Node::start(slot, &redis, &history).await);
    }

    let started = Instant::now();
    let mut faults = FAULTS.iter().cycle();

    while started.elapsed() < chaos {
        let fault = faults.next().unwrap();
        let begun = Instant::now();
        let victim = history.random(NODES);

        println!(
            "{:>4}s  {fault:?} (node {victim})",
            started.elapsed().as_secs()
        );
        match fault {
            Fault::RestartServer => {
                redis.stop().await;
                tokio::time::sleep(Duration::from_secs(3)).await;
                redis.restart().await;
            }
            Fault::FreezeServer => {
                redis.pause().await;
                tokio::time::sleep(Duration::from_secs(3)).await;
                redis.unpause().await;
            }
            Fault::CutNode => {
                nodes[victim].proxy.cut();
                tokio::time::sleep(Duration::from_secs(10)).await;
                nodes[victim].proxy.restore();
            }
            Fault::CrashNode => {
                // First, so the slot's keys never have two writers.
                drop(nodes.remove(victim));
                nodes.insert(victim, Node::start(victim, &redis, &history).await);
            }
            Fault::DropSubscriptions => {
                redis.drop_subscriptions().await;
            }
        }
        tokio::time::sleep(PERIOD.saturating_sub(begun.elapsed())).await;
    }

    println!("{:>4}s  quiet", started.elapsed().as_secs());
    tokio::time::sleep(BOUND + Duration::from_secs(5)).await;

    for (slot, node) in nodes.iter().enumerate() {
        assert!(
            node.store.local_reads_enabled(),
            "node {slot} reads locally again"
        );
        let read = node.store.get::<u64>("n0:k0").await;
        assert!(read.is_ok(), "node {slot} reads again: {read:?}");
    }
    drop(nodes);

    provider.force_flush().unwrap();
    for (name, attributes, value) in sums(&exporter) {
        println!("{name}{attributes:?} = {value}");
    }
    history.report();
}

#[derive(Debug, Clone, Copy)]
enum Fault {
    RestartServer,
    CutNode,
    FreezeServer,
    CrashNode,
    DropSubscriptions,
}

const FAULTS: [Fault; 5] = [
    Fault::RestartServer,
    Fault::CutNode,
    Fault::FreezeServer,
    Fault::CrashNode,
    Fault::DropSubscriptions,
];

/// One node: a hybrid store reaching the server through a proxy of its own, and the tasks that
/// write and read through it, all on its own runtime. Dropping it stops the runtime where it is,
/// as a process crash would, writes in flight included.
struct Node {
    runtime: Option<Runtime>,
    store: RedisHybridCacheStore,
    proxy: Proxy,
}

impl Node {
    /// Starts a node that writes the keys of `slot`, taking over from a node that crashed.
    async fn start(slot: usize, redis: &Redis, history: &Arc<History>) -> Self {
        let proxy = Proxy::start(redis.url()).await;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let url = proxy.url().to_owned();
        let store = runtime
            .spawn(async move {
                RedisHybridCacheStore::connect("chaos", url, &HybridCacheSettings::default()).await
            })
            .await
            .unwrap()
            .unwrap();

        runtime.spawn(write(slot, store.clone(), Arc::clone(history)));
        for _ in 0..2 {
            runtime.spawn(read(store.clone(), Arc::clone(history)));
        }

        Self {
            runtime: Some(runtime),
            store,
            proxy,
        }
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        // Not a plain drop, which blocks and so panics inside the test's runtime.
        self.runtime.take().unwrap().shutdown_background();
    }
}

/// Writes the keys of one slot, one call at a time, so each key's writes have one order.
async fn write(slot: usize, store: RedisHybridCacheStore, history: Arc<History>) {
    loop {
        let roll = history.random(100);
        let keys: Vec<String> = if roll < 1 {
            (0..KEYS).map(|key| format!("n{slot}:k{key}")).collect()
        } else {
            vec![format!("n{slot}:k{}", history.random(KEYS))]
        };
        let version = (roll >= 10).then(|| history.next_version());
        let started = history.begin(&keys, version);
        let ok = match (version, keys.as_slice()) {
            (Some(version), [key]) => store.set(key, version, None).await.is_ok(),
            (None, [key]) => store.remove(key).await.is_ok(),
            _ => store.remove_prefix(&format!("n{slot}")).await.is_ok(),
        };

        history.finish(&keys, started, ok);
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// Reads random keys of every slot and checks each value it gets.
async fn read(store: RedisHybridCacheStore, history: Arc<History>) {
    loop {
        let key = format!("n{}:k{}", history.random(NODES), history.random(KEYS));
        let started = Instant::now();

        match store.get::<u64>(&key).await {
            Ok(Some(version)) => history.check(&key, version, started),
            Ok(None) => history.count(&history.misses),
            Err(_) => history.count(&history.errors),
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

/// Every write of every key, and what the reads saw.
#[derive(Default)]
struct History {
    keys: Mutex<HashMap<String, Vec<Write>>>,
    versions: AtomicU64,
    seed: AtomicU64,
    stale: Mutex<Vec<String>>,
    max_stale: Mutex<Duration>,
    hits: AtomicU64,
    misses: AtomicU64,
    errors: AtomicU64,
    late_failed: AtomicU64,
    writes_ok: AtomicU64,
    writes_failed: AtomicU64,
}

/// One write: the version it stored (`None` for a removal) and, once it returned `Ok`, when.
struct Write {
    version: Option<u64>,
    done: Option<Instant>,
}

impl History {
    fn next_version(&self) -> u64 {
        self.versions.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// A number below `below`, from a shared xorshift.
    fn random(&self, below: usize) -> usize {
        let mut x = self
            .seed
            .fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed)
            | 1;

        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        (x % below as u64) as usize
    }

    fn count(&self, counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a write starting on `keys`, and returns where it sits in each key's history.
    fn begin(&self, keys: &[String], version: Option<u64>) -> Vec<usize> {
        let mut histories = self.keys.lock().unwrap();

        keys.iter()
            .map(|key| {
                let writes = histories.entry(key.clone()).or_default();

                writes.push(Write {
                    version,
                    done: None,
                });
                writes.len() - 1
            })
            .collect()
    }

    /// Marks the write `Ok` from now. A failed write stays without a time: the server may still
    /// have applied it, now or later.
    fn finish(&self, keys: &[String], started: Vec<usize>, ok: bool) {
        if !ok {
            self.count(&self.writes_failed);
            return;
        }

        let now = Instant::now();
        let mut histories = self.keys.lock().unwrap();

        for (key, index) in keys.iter().zip(started) {
            histories.get_mut(key).unwrap()[index].done = Some(now);
        }
        self.count(&self.writes_ok);
    }

    /// Checks a read of `version` that started at `started`: a write that returned `Ok` after
    /// the one that stored it must have done so less than `BOUND` before the read.
    fn check(&self, key: &str, version: u64, started: Instant) {
        self.count(&self.hits);

        let histories = self.keys.lock().unwrap();
        let writes = &histories[key];
        let index = writes
            .iter()
            .position(|write| write.version == Some(version))
            .expect("a version this test wrote");
        let replaced = writes[index + 1..]
            .iter()
            .filter_map(|write| write.done)
            .filter(|done| *done <= started)
            .min();
        let Some(replaced) = replaced else { return };

        if writes[index].done.is_none() {
            // A failed write that reached the server late, which no store can order.
            self.count(&self.late_failed);
            return;
        }

        let stale = started - replaced;
        let mut max = self.max_stale.lock().unwrap();

        *max = (*max).max(stale);
        if stale > BOUND {
            self.stale.lock().unwrap().push(format!(
                "{key}: version {version} read {stale:?} after it was replaced"
            ));
        }
    }

    fn report(&self) {
        let load = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        let stale = self.stale.lock().unwrap();

        println!(
            "reads: {} hits, {} misses, {} errors; writes: {} ok, {} failed; \
             {} reads of a failed write that landed late; longest stale read {:?}",
            load(&self.hits),
            load(&self.misses),
            load(&self.errors),
            load(&self.writes_ok),
            load(&self.writes_failed),
            load(&self.late_failed),
            *self.max_stale.lock().unwrap(),
        );
        assert!(load(&self.hits) > 0, "the nodes read something");
        assert!(
            stale.is_empty(),
            "stale reads past {BOUND:?}:\n{}",
            stale.join("\n")
        );
    }
}

/// Every counter and gauge the stores recorded, with its attributes other than `store`.
fn sums(exporter: &InMemoryMetricExporter) -> Vec<(String, Vec<String>, u64)> {
    let exported = exporter.get_finished_metrics().unwrap();
    let mut latest = HashMap::new();

    for metric in exported
        .iter()
        .flat_map(|resource| resource.scope_metrics())
        .flat_map(|scope| scope.metrics())
    {
        let points: Vec<_> = match metric.data() {
            AggregatedMetrics::U64(MetricData::Sum(sum)) => sum
                .data_points()
                .map(|point| {
                    (
                        point.attributes().cloned().collect::<Vec<_>>(),
                        point.value(),
                    )
                })
                .collect(),
            _ => continue,
        };

        for (attributes, value) in points {
            let attributes = attributes
                .iter()
                .filter(|kv| kv.key.as_str() != "store")
                .map(|kv| format!("{}={}", kv.key, kv.value))
                .collect();

            latest.insert((metric.name().to_owned(), attributes), value);
        }
    }

    let mut sums: Vec<_> = latest
        .into_iter()
        .map(|((name, attributes), value)| (name, attributes, value))
        .collect();

    sums.sort();
    sums
}
