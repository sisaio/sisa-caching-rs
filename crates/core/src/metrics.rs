//! The metrics the stores record, through the [OpenTelemetry](https://opentelemetry.io) metrics
//! API under the meter `sisa-caching`. Nothing is recorded until the application installs an
//! OpenTelemetry SDK meter provider, for example one that pushes over OTLP. Install it before the
//! first cache call: the instruments are created then, from the provider installed at that time.
//!
//! | Metric | Type | What it counts |
//! | --- | --- | --- |
//! | `sisa.caching.hits` | counter | Reads that found a live entry |
//! | `sisa.caching.misses` | counter | Reads that found nothing, or only an expired entry |
//! | `sisa.caching.load.duration` | histogram, seconds | How long each `get_or_set` loader ran, with `outcome` `ok` or `error` |
//! | `sisa.caching.errors` | counter | Store calls inside `get_or_set` that failed, with `operation` `get` for a read or `set` for storing what it loaded |
//! | `sisa.caching.evictions` | counter | Entries a full memory store dropped to make room |
//! | `sisa.caching.invalidations.received` | counter | Other nodes' invalidations a hybrid store applied |
//! | `sisa.caching.publish.failures` | counter | Invalidations a hybrid store couldn't publish after 3 tries |
//! | `sisa.caching.backplane.connected` | gauge | 1 while a hybrid store's backplane is subscribed, else 0 |
//! | `sisa.caching.backplane.unreadable` | counter | Backplane messages a hybrid store couldn't read, each of which cleared its local layer |
//!
//! Every metric has a `store` attribute with the store's name. Hits and misses also have a
//! `layer` attribute: `memory`, `redis`, or `hybrid` for the hybrid store as a whole. A hybrid
//! store records all three, so its local hit rate is the `memory` series and its overall one the
//! `hybrid` series.
//!
//! A `get_or_set` that has to load reads before and after taking its lock, so it counts two
//! misses. Count loads with the load histogram's count instead.
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

use std::{sync::LazyLock, time::Duration};

use opentelemetry::{
    KeyValue, Value, global,
    metrics::{Counter, Gauge, Histogram},
};

use crate::cache::KeyPrefix;

/// The instruments every store records into.
#[doc(hidden)]
pub static METRICS: LazyLock<CacheMetrics> = LazyLock::new(|| {
    let meter = global::meter("sisa-caching");

    CacheMetrics {
        hits: meter
            .u64_counter("sisa.caching.hits")
            .with_description("Reads that found a live entry")
            .build(),
        misses: meter
            .u64_counter("sisa.caching.misses")
            .with_description("Reads that found nothing, or only an expired entry")
            .build(),
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

/// The instruments, each recorded through one method so the attribute names are written once.
#[doc(hidden)]
#[derive(Debug)]
pub struct CacheMetrics {
    hits: Counter<u64>,
    misses: Counter<u64>,
    load_duration: Histogram<f64>,
    errors: Counter<u64>,
    evictions: Counter<u64>,
    invalidations_received: Counter<u64>,
    publish_failures: Counter<u64>,
    backplane_connected: Gauge<u64>,
    backplane_unreadable: Counter<u64>,
}

impl CacheMetrics {
    /// Counts one read as a hit when it `found` a live entry, else as a miss.
    ///
    /// Hits and misses count reads: `get_entry`, and `get` and `get_or_set` through it. A
    /// `get_or_set` that has to load reads before and after taking its lock, so it counts two
    /// misses; the count of the load duration is the number of loads.
    pub fn read(&self, store: &KeyPrefix, layer: Layer, found: bool) {
        let counter = if found { &self.hits } else { &self.misses };

        counter.add(
            1,
            &[
                store_attribute(store),
                KeyValue::new("layer", layer.as_str()),
            ],
        );
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
}

/// The `store` attribute, sharing the name's string rather than copying it on every update.
fn store_attribute(store: &KeyPrefix) -> KeyValue {
    KeyValue::new("store", Value::String(store.shared().into()))
}
