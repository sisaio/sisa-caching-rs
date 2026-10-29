//! The metrics the stores record, through the [OpenTelemetry](https://opentelemetry.io) metrics
//! API under the meter `sisa-caching`. Nothing is recorded until the application installs an
//! OpenTelemetry SDK meter provider, for example one that pushes over OTLP. Install it before
//! building the first store: the instruments are created then, from the provider installed at
//! that time.
//!
//! | Metric | Type | What it counts |
//! | --- | --- | --- |
//! | `sisa.caching.hits` | counter | Reads that found a live entry |
//! | `sisa.caching.misses` | counter | Reads that returned no value: nothing there, an expired entry, or a value that doesn't read as the requested type |
//! | `sisa.caching.load.duration` | histogram, seconds | How long each `get_or_set` loader ran, with `outcome` `ok` or `error` |
//! | `sisa.caching.errors` | counter | Store calls inside `get_or_set` that failed, with `operation` `get` for a read or `set` for storing what it loaded |
//! | `sisa.caching.evictions` | counter | Entries a full memory store dropped to make room |
//! | `sisa.caching.invalidations.received` | counter | Other nodes' invalidations a hybrid store applied |
//! | `sisa.caching.invalidations.delivered` | counter | Subscribers the server delivered a hybrid store's invalidations to, as `PUBLISH` reports, the publishing node's own included |
//! | `sisa.caching.publish.failures` | counter | Invalidations a hybrid store couldn't publish after 3 tries |
//! | `sisa.caching.backplane.connected` | gauge | 1 while a hybrid store's backplane is subscribed, else 0 |
//! | `sisa.caching.backplane.unreadable` | counter | Backplane messages a hybrid store couldn't read, each of which cleared its local layer |
//! | `sisa.caching.local.clears` | counter | Times a hybrid store cleared its local layer because it may have missed an invalidation, with `reason`: `gap` for a number missing below a heartbeat, `untracked` for numbers it can't track, `peer_lost` for a node whose heartbeats stopped, `resubscribed` for a new subscription after a lost one, or `unreadable` for a message it couldn't read. A clear another node asked for isn't counted |
//!
//! Every metric has a `store` attribute with the store's name. Hits and misses also have a
//! `layer` attribute: `memory`, `redis`, or `hybrid` for the hybrid store as a whole. A hybrid
//! store records all three, so its local hit rate is the `memory` series and its overall one the
//! `hybrid` series.
//!
//! A `get_or_set` that has to load reads before and after taking its lock, so it counts two
//! misses. Count loads with the load histogram's count instead.
//!
//! `errors` counts only the calls `get_or_set` makes, whose failures it handles itself: a failed
//! read loads, and a failed store returns the loaded value without caching it. With the store
//! down, each `get_or_set` counts two `get` errors, one per read, and a `set` error. A call the
//! application makes returns its error instead.
//!
//! `invalidations.delivered` grows by the number of listening nodes for each invalidation. On a
//! cluster, `SPUBLISH` counts only the subscribers on the node that took it.
//!
//! A setup at startup, with the OpenTelemetry SDK and an OTLP exporter:
//!
//! ```ignore
//! use opentelemetry::global;
//! use opentelemetry_sdk::{metrics::SdkMeterProvider, propagation::TraceContextPropagator};
//!
//! let exporter = opentelemetry_otlp::MetricExporter::builder().with_http().build()?;
//! global::set_meter_provider(SdkMeterProvider::builder().with_periodic_exporter(exporter).build());
//! // For hybrid stores, so traces follow invalidations to other nodes.
//! global::set_text_map_propagator(TraceContextPropagator::new());
//! ```

use std::{
    sync::{
        LazyLock, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use opentelemetry::{
    KeyValue, Value, global,
    metrics::{Counter, Gauge, Histogram},
};

use crate::cache::KeyPrefix;

/// The instruments every store records into.
#[doc(hidden)]
pub static METRICS: LazyLock<CacheMetrics> = LazyLock::new(|| {
    let meter = global::meter("sisa-caching");

    // An SDK update per read would cost more than the read, so the stores count their reads and
    // these report the counts when the meter provider collects.
    for (name, description, hits) in [
        ("sisa.caching.hits", "Reads that found a live entry", true),
        ("sisa.caching.misses", "Reads that returned no value", false),
    ] {
        meter
            .u64_observable_counter(name)
            .with_description(description)
            .with_callback(move |observer| {
                for reads in READS.lock().unwrap_or_else(|e| e.into_inner()).iter() {
                    let count = if hits { &reads.hits } else { &reads.misses };

                    observer.observe(count.sum(), &reads.attributes);
                }
            })
            .build();
    }

    CacheMetrics {
        load_duration: meter
            .f64_histogram("sisa.caching.load.duration")
            .with_unit("s")
            .with_description("How long each get_or_set loader ran")
            .with_boundaries(vec![
                0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
            ])
            .build(),
        errors: meter
            .u64_counter("sisa.caching.errors")
            .with_description("Store calls inside get_or_set that failed")
            .build(),
        evictions: meter
            .u64_counter("sisa.caching.evictions")
            .with_description("Entries a full memory store dropped to make room")
            .build(),
        invalidations_received: meter
            .u64_counter("sisa.caching.invalidations.received")
            .with_description("Invalidations from other nodes that a hybrid store applied")
            .build(),
        invalidations_delivered: meter
            .u64_counter("sisa.caching.invalidations.delivered")
            .with_description("Subscribers the server delivered a hybrid store's invalidations to")
            .build(),
        publish_failures: meter
            .u64_counter("sisa.caching.publish.failures")
            .with_description("Invalidations a hybrid store could not publish after every retry")
            .build(),
        backplane_connected: meter
            .u64_gauge("sisa.caching.backplane.connected")
            .with_description("1 while a hybrid store's backplane is subscribed, else 0")
            .build(),
        backplane_unreadable: meter
            .u64_counter("sisa.caching.backplane.unreadable")
            .with_description("Backplane messages a hybrid store could not read")
            .build(),
        local_clears: meter
            .u64_counter("sisa.caching.local.clears")
            .with_description("Times a hybrid store cleared its local layer, as it may have missed an invalidation")
            .build(),
    }
});

/// Which part of a store a hit or miss was counted in.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Layer {
    Memory,
    Redis,
    /// A hybrid store as a whole: a hit in either of its layers.
    Hybrid,
}

impl Layer {
    fn as_str(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Redis => "redis",
            Self::Hybrid => "hybrid",
        }
    }
}

/// The read counts of every store layer, which the hit and miss counters report.
static READS: Mutex<Vec<&'static Reads>> = Mutex::new(Vec::new());

/// One store layer's hits and misses, with the attributes they are reported under.
#[doc(hidden)]
#[derive(Debug)]
pub struct Reads {
    store: KeyPrefix,
    layer: Layer,
    attributes: [KeyValue; 2],
    hits: Striped,
    misses: Striped,
}

impl Reads {
    /// Counts one read as a hit when it `found` a live entry, else as a miss, as is a value that
    /// doesn't read as the requested type, since the caller gets none.
    ///
    /// Hits and misses count reads: `get_entry`, and `get` and `get_or_set` through it. A
    /// `get_or_set` that has to load reads before and after taking its lock, so it counts two
    /// misses; the count of the load duration is the number of loads.
    pub fn record(&self, found: bool) {
        if found { &self.hits } else { &self.misses }.add();
    }
}

const STRIPES: usize = 16;

/// A count split over `STRIPES` cache lines, so threads that count at once rarely share one.
/// Each thread adds to the stripe it was given on its first count.
#[derive(Debug, Default)]
struct Striped([Stripe; STRIPES]);

/// 128 bytes, as some CPUs fetch cache lines in pairs.
#[derive(Debug, Default)]
#[repr(align(128))]
struct Stripe(AtomicU64);

impl Striped {
    fn add(&self) {
        static NEXT: AtomicUsize = AtomicUsize::new(0);

        thread_local! {
            static STRIPE: usize = NEXT.fetch_add(1, Ordering::Relaxed) % STRIPES;
        }

        let stripe = STRIPE.with(|stripe| *stripe);

        self.0[stripe].0.fetch_add(1, Ordering::Relaxed);
    }

    fn sum(&self) -> u64 {
        self.0
            .iter()
            .map(|stripe| stripe.0.load(Ordering::Relaxed))
            .sum()
    }
}

/// The instruments, each recorded through one method so the attribute names are written once.
#[doc(hidden)]
#[derive(Debug)]
pub struct CacheMetrics {
    load_duration: Histogram<f64>,
    errors: Counter<u64>,
    evictions: Counter<u64>,
    invalidations_received: Counter<u64>,
    invalidations_delivered: Counter<u64>,
    publish_failures: Counter<u64>,
    backplane_connected: Gauge<u64>,
    backplane_unreadable: Counter<u64>,
    local_clears: Counter<u64>,
}

impl CacheMetrics {
    /// The hit and miss counts of `store`'s `layer`, which a store takes when it is built. Stores
    /// with the same name share them, as they share a series, and they are never freed, so a
    /// rebuilt store's series counts on instead of starting over.
    pub fn reads(&self, store: &KeyPrefix, layer: Layer) -> &'static Reads {
        let mut all = READS.lock().unwrap_or_else(|e| e.into_inner());

        if let Some(reads) = all
            .iter()
            .find(|reads| reads.store == *store && reads.layer == layer)
        {
            return reads;
        }

        let reads = Box::leak(Box::new(Reads {
            store: store.clone(),
            layer,
            attributes: [
                store_attribute(store),
                KeyValue::new("layer", layer.as_str()),
            ],
            hits: Striped::default(),
            misses: Striped::default(),
        }));

        all.push(reads);

        reads
    }

    /// Records how long one `get_or_set` loader ran, with `outcome` `ok` or `error`.
    pub fn load(&self, store: &KeyPrefix, duration: Duration, ok: bool) {
        self.load_duration.record(
            duration.as_secs_f64(),
            &[
                store_attribute(store),
                KeyValue::new("outcome", if ok { "ok" } else { "error" }),
            ],
        );
    }

    /// Counts one store call inside `get_or_set` that failed: `get` for a read, `set` for storing
    /// what it loaded.
    pub(crate) fn failed(&self, store: &KeyPrefix, operation: &'static str) {
        self.errors.add(
            1,
            &[
                store_attribute(store),
                KeyValue::new("operation", operation),
            ],
        );
    }

    /// Counts entries a full memory store dropped to make room. Growing fast means `max_entries`
    /// is too small for the working set.
    pub fn evicted(&self, store: &KeyPrefix, count: usize) {
        self.evictions.add(count as u64, &[store_attribute(store)]);
    }

    /// Counts one invalidation from another node that a hybrid store applied.
    pub fn invalidation_received(&self, store: &KeyPrefix) {
        self.invalidations_received
            .add(1, &[store_attribute(store)]);
    }

    /// Counts the subscribers the server delivered one of a hybrid store's invalidations to, as
    /// `PUBLISH` reports: every subscriber of the channel, this node's own included. It drops when
    /// a node stops listening.
    pub fn invalidation_delivered(&self, store: &KeyPrefix, subscribers: usize) {
        self.invalidations_delivered
            .add(subscribers as u64, &[store_attribute(store)]);
    }

    /// Counts one invalidation a hybrid store could not publish after every retry. Other nodes
    /// clear their local layers at the store's next heartbeat.
    pub fn publish_failed(&self, store: &KeyPrefix) {
        self.publish_failures.add(1, &[store_attribute(store)]);
    }

    /// Sets whether a hybrid store's backplane worker is subscribed and local reads are on.
    pub fn backplane_connected(&self, store: &KeyPrefix, connected: bool) {
        self.backplane_connected
            .record(connected.into(), &[store_attribute(store)]);
    }

    /// Counts one backplane message a hybrid store couldn't read. Each clears its local layer.
    pub fn backplane_unreadable(&self, store: &KeyPrefix) {
        self.backplane_unreadable.add(1, &[store_attribute(store)]);
    }

    /// Counts one clear of a hybrid store's local layer, done because it may have missed an
    /// invalidation, with `reason`: `gap`, `untracked`, `peer_lost`, `resubscribed` or
    /// `unreadable`. A clear another node asked for isn't counted.
    pub fn local_cleared(&self, store: &KeyPrefix, reason: &'static str) {
        self.local_clears.add(
            1,
            &[store_attribute(store), KeyValue::new("reason", reason)],
        );
    }
}

/// The `store` attribute, sharing the name's string rather than copying it on every update.
fn store_attribute(store: &KeyPrefix) -> KeyValue {
    KeyValue::new("store", Value::String(store.shared().into()))
}
