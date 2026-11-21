//! Traces and metrics pushed over OTLP to an OpenTelemetry collector, and logs on stdout, all
//! from one `sisa_telemetry::init`.

use sisa_telemetry::{
    LogFormat, OtlpProtocol, ServiceResource, TelemetryError, TelemetryGuard, TelemetrySettings,
};

/// Pushes traces and metrics as `service` over OTLP/HTTP to `OTEL_EXPORTER_OTLP_ENDPOINT`
/// (`http://localhost:4318` by default), and prints logs to stdout. `PORT` becomes the
/// `service.instance.id`, so two instances of one service stay apart. Call it once, first thing
/// in `main`, before building the stores, so their metrics go to this meter provider, and shut
/// the guard down on exit to send what is left.
///
/// `RUST_LOG` filters the logs and the traces (`info` by default; `sisa_caching=debug` adds a
/// span per store call). `sisa-telemetry` doesn't read it, so this reads it into the settings.
///
/// It also installs the W3C trace-context propagator: hybrid invalidations carry the writer's
/// trace as `traceparent`, so a write on one instance and its eviction on the others show up as
/// one trace.
///
/// `sisa-telemetry` 0.1.0 exports no logs over OTLP and has no TLS, so the logs are on stdout
/// only and the collector is reached over plain HTTP.
pub fn init_telemetry(service: &'static str) -> Result<TelemetryGuard, TelemetryError> {
    let port = std::env::var("PORT").unwrap_or_else(|_| "3000".into());

    let mut settings = TelemetrySettings::default();
    settings.otlp.protocol = OtlpProtocol::HttpProtobuf;
    settings.log.format = LogFormat::Pretty;
    settings.log.filter = std::env::var("RUST_LOG")
        .ok()
        .filter(|filter| !filter.is_empty())
        .unwrap_or_else(|| "info".into());

    sisa_telemetry::init(
        &settings,
        ServiceResource::new(service, env!("CARGO_PKG_VERSION"))
            .with_attribute("service.instance.id", port),
    )
}
