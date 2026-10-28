# Observability

The stores trace through `tracing` and record metrics through the OpenTelemetry metrics API. They
don't export anything themselves: install an OpenTelemetry SDK, for example with an OTLP exporter,
and a `tracing` subscriber with the `tracing-opentelemetry` layer. The
[examples](../examples/users-api) set up all three signals.

Install the meter provider before the first cache call. The stores create their instruments on
that call, from the provider installed then, so a provider installed later never sees them.

## Metrics

All under the meter `sisa-caching`. Every metric has a `store` attribute with the store's name.

| Metric | Type | Extra attributes | What it counts |
| --- | --- | --- | --- |
| `sisa.caching.hits` | counter | `layer` | Reads that found a live entry |
| `sisa.caching.misses` | counter | `layer` | Reads that found nothing, or only an expired entry |
| `sisa.caching.load.duration` | histogram, seconds | `outcome`: `ok` or `error` | How long each `get_or_set` loader ran |
| `sisa.caching.evictions` | counter | | Entries a full memory store dropped to make room |
| `sisa.caching.invalidations.received` | counter | | Other nodes' invalidations a hybrid store applied |
| `sisa.caching.publish.failures` | counter | | Invalidations a hybrid store couldn't publish after 3 tries |
| `sisa.caching.backplane.connected` | gauge | | 1 while a hybrid store's backplane is subscribed, else 0 |

`layer` is `memory`, `redis`, or `hybrid` for a hybrid store as a whole. A hybrid store records
all three, so its local hit rate is the `memory` series and its overall hit rate the `hybrid`
series.

A `get_or_set` that has to load reads before and after taking its lock, so it counts two misses.
Count loads with the histogram's count instead.

Worth an alert:
- `publish.failures` above 0: other nodes may serve stale values for up to `max_ttl_secs`.
- `backplane.connected` at 0: that node reads everything from Redis.
- `evictions` growing fast: `max_entries` is smaller than the working set.

## Spans

| Span | Level | Fields |
| --- | --- | --- |
| `hybrid write` | info | `store`, `kind` |
| `hybrid apply_invalidation` | info | `store`, `kind`; a child of the writer's `hybrid write` span |
| `hybrid resync` | info | `store`, `connected` |
| `cache get_or_set` | debug | `key` |
| `memory get`, `memory set`, `memory remove` | debug | `key` |
| `memory remove_prefix`, `memory clear`, `memory purge` | debug | `store`, plus `prefix` or `removed` |
| `redis get`, `redis set`, `redis remove` | debug | `key` |
| `redis remove_prefix`, `redis clear` | debug | `store`, plus `prefix` |
| `hybrid get` | debug | `key` |

`kind` is `key`, `prefix` or `clear`. Info spans and warnings never carry a key, so they are safe
to keep in production; only debug spans carry the full key. Failed Redis and hybrid calls mark
their span as an error.

A hybrid write and the `hybrid apply_invalidation` spans it causes on other nodes share one trace.
This needs a text map propagator, such as `TraceContextPropagator`, installed at startup. It also
needs the `tracing-opentelemetry` layer's context activation, which is on by default: the writer
sends OpenTelemetry's current context, and that holds the span only while activation is on. With
`with_context_activation(false)`, invalidations carry no trace context, and nothing warns.

## Logs

The stores log only errors they handle themselves, as warnings: a loaded value that couldn't be
stored, a key that couldn't be read so the loaded value is stored over it, a refresh whose load
failed while the cached value was still live, which serves that value, a lock wait that
timed out, an invalidation that couldn't be published, and a backplane subscription lost or
failing. The backplane worker also logs at info when it subscribes, and a `WorkerHost` logs each
worker starting and stopping at info, and a panic at error.

Warnings name the store, not the key. A `get_or_set` warning fires inside the `cache get_or_set`
debug span, so with debug on it shows next to the key. An error's text can still quote part of a
value, such as a string that no longer decodes into the requested type.

Every target starts with `sisa_caching`, so one filter covers all crates:

```sh
RUST_LOG=info,sisa_caching=debug
```
