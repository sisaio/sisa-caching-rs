//! Logs, traces and metrics, pushed over OTLP to an OpenTelemetry collector.

use opentelemetry::{KeyValue, global, trace::TracerProvider};
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::{LogExporter, MetricExporter, SpanExporter};
use opentelemetry_sdk::{
    Resource, logs::SdkLoggerProvider, metrics::SdkMeterProvider,
    propagation::TraceContextPropagator, trace::SdkTracerProvider,
};
use tracing_subscriber::{EnvFilter, filter::filter_fn, prelude::*};

/// The providers that batch and push telemetry. Shut them down on exit to send what is left.
pub struct Telemetry {
    tracer_provider: SdkTracerProvider,
    meter_provider: SdkMeterProvider,
    logger_provider: SdkLoggerProvider,
}

impl Telemetry {
    pub fn shutdown(self) {
        _ = self.tracer_provider.shutdown();
        _ = self.meter_provider.shutdown();
        _ = self.logger_provider.shutdown();
    }
}

/// Pushes traces, metrics and logs as `service` over OTLP/HTTP to `OTEL_EXPORTER_OTLP_ENDPOINT`
/// (`http://localhost:4318` by default), and prints logs to stderr. `RUST_LOG` filters both
/// (`info` by default; `sisa_caching=debug` adds a span per store call). `PORT` becomes the
/// `service.instance.id`, so two instances of one service stay apart. Call it once, before
/// building the stores, so their metrics go to this meter provider.
pub fn init_telemetry(service: &'static str) -> Result<Telemetry, Box<dyn std::error::Error>> {
    let port = std::env::var("PORT").unwrap_or_else(|_| "3000".into());
    let resource = Resource::builder()
        .with_service_name(service)
        .with_attribute(KeyValue::new("service.instance.id", port))
        .build();
    let tracer_provider = SdkTracerProvider::builder()
        .with_resource(resource.clone())
        .with_batch_exporter(SpanExporter::builder().with_http().build()?)
        .build();
    let meter_provider = SdkMeterProvider::builder()
        .with_resource(resource.clone())
        .with_periodic_exporter(MetricExporter::builder().with_http().build()?)
        .build();
    let logger_provider = SdkLoggerProvider::builder()
        .with_resource(resource)
        .with_batch_exporter(LogExporter::builder().with_http().build()?)
        .build();

    global::set_meter_provider(meter_provider.clone());
    // Hybrid invalidations carry the writer's trace as W3C `traceparent`, so a write on one
    // instance and its eviction on the others show up as one trace.
    global::set_text_map_propagator(TraceContextPropagator::new());

    // The exporters log through `tracing` too; sending those logs over OTLP would loop.
    let not_the_exporter = filter_fn(|metadata| {
        !["opentelemetry", "hyper", "reqwest", "h2"]
            .iter()
            .any(|target| metadata.target().starts_with(target))
    });

    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(
            tracing_opentelemetry::layer()
                .with_tracer(tracer_provider.tracer(service))
                .with_filter(not_the_exporter.clone()),
        )
        .with(OpenTelemetryTracingBridge::new(&logger_provider).with_filter(not_the_exporter))
        .init();

    Ok(Telemetry {
        tracer_provider,
        meter_provider,
        logger_provider,
    })
}
