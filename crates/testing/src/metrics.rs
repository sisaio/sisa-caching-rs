//! What the stores record, read back.

use opentelemetry::{KeyValue, global};
use opentelemetry_sdk::metrics::{
    InMemoryMetricExporter, PeriodicReader, SdkMeterProvider,
    data::{AggregatedMetrics, MetricData},
};

/// The global meter provider, with an exporter that keeps everything the stores record.
///
/// Stores create their instruments when the first one is built, from the provider installed then,
/// so a test that reads metrics runs in a test binary of its own and installs this first.
pub struct Metrics {
    exporter: InMemoryMetricExporter,
    provider: SdkMeterProvider,
}

impl Metrics {
    pub fn install() -> Self {
        let exporter = InMemoryMetricExporter::default();
        let provider = SdkMeterProvider::builder()
            .with_reader(PeriodicReader::builder(exporter.clone()).build())
            .build();

        global::set_meter_provider(provider.clone());

        Self { exporter, provider }
    }

    /// The latest value of the counter or gauge `name`, or the count of the histogram `name`, with
    /// the attributes `store=app` and `attributes`, and no others; 0 when it was never recorded.
    pub fn value(&self, name: &str, attributes: &[KeyValue]) -> u64 {
        self.provider.force_flush().unwrap();

        let wanted = [&[KeyValue::new("store", "app")], attributes].concat();
        let matches = |found: Vec<&KeyValue>| {
            found.len() == wanted.len() && wanted.iter().all(|kv| found.contains(&kv))
        };

        self.exporter
            .get_finished_metrics()
            .unwrap()
            .iter()
            .flat_map(|resource| resource.scope_metrics())
            .flat_map(|scope| scope.metrics())
            .filter(|metric| metric.name() == name)
            .flat_map(|metric| -> Vec<u64> {
                match metric.data() {
                    AggregatedMetrics::U64(MetricData::Sum(sum)) => sum
                        .data_points()
                        .filter(|point| matches(point.attributes().collect()))
                        .map(|point| point.value())
                        .collect(),
                    AggregatedMetrics::U64(MetricData::Gauge(gauge)) => gauge
                        .data_points()
                        .filter(|point| matches(point.attributes().collect()))
                        .map(|point| point.value())
                        .collect(),
                    AggregatedMetrics::F64(MetricData::Histogram(histogram)) => histogram
                        .data_points()
                        .filter(|point| matches(point.attributes().collect()))
                        .map(|point| point.count())
                        .collect(),
                    _ => Vec::new(),
                }
            })
            .last()
            .unwrap_or_default()
    }
}
