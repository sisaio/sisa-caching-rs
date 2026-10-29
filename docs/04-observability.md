# Observability

The stores trace through `tracing` and record metrics through the OpenTelemetry metrics API. They
don't export anything themselves: install an OpenTelemetry SDK, for example with an OTLP exporter,
and a `tracing` subscriber with the `tracing-opentelemetry` layer. The
[examples](../examples/users-api) set up all three signals.

Install the meter provider before the first cache call. The stores create their instruments on
that call, from the provider installed then, so a provider installed later never sees them.

## Metrics

All under the meter `sisa-caching`, each with a `store` attribute naming the store. The
[`metrics` module docs](https://docs.rs/sisa-caching-core/latest/sisa_caching_core/metrics/) list
every metric with its attributes and what it counts.

Worth an alert:
- `errors` above 0: the store can't be reached, or holds data the code can't read, and every
  `get_or_set` meanwhile runs its loader.
- `publish.failures` above 0: other nodes clear their local layers at the writer's next heartbeat,
  so hit rates dip.
- `invalidations.delivered` falling while writes go on: a node stopped listening.
- `backplane.connected` at 0: that node reads everything from Redis.
- `backplane.unreadable` above 0: something publishes messages this release can't read, and each
  one clears the local layer.
- `local.clears` growing: each clear empties a node's local layer, so its hit rate dips; `reason`
  says why.
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

The stores warn only about errors they handle themselves. A few info lines mark workers and
recoveries, and a worker's panic logs an error.

`get_or_set` handles its failed store calls: a key it couldn't read, so it loads and stores the
loaded value over it, and a loaded value it couldn't store. The first failure of a run logs a
warning with the store, the `operation` (`get` or `set`) and the error, and the next success of
that operation logs `store calls work again` at info. So an outage logs two lines per store and
operation, however many calls fail, while `sisa.caching.errors` counts each failure. A failure
that comes and goes, such as one key holding data that no longer decodes, logs a pair each time.

The other warnings fire every time: a refresh whose load failed while the cached value was still
live, which serves that value, a lock wait that timed out, an invalidation that couldn't be
published, a backplane subscription lost or failing, a backplane message that couldn't be read, and
a local layer cleared because an invalidation went missing or a node's heartbeats stopped. When a
node stops without calling `shutdown`, every other node logs that clear `heartbeat_timeout_ms`
later, so such a rolling deploy logs it once for each node it replaces. The backplane worker also logs at info when
it subscribes. Each store's worker logs starting and stopping at info, and a panic at error, with
the worker's name in `worker` and the store's in `store`.

Warnings name the store, not the key. A `get_or_set` warning fires inside the `cache get_or_set`
debug span, so with debug on it shows next to the key. An error's text can still quote part of a
value, such as a string that no longer decodes into the requested type.

Every target starts with `sisa_caching`, so one filter covers all crates:

```sh
RUST_LOG=info,sisa_caching=debug
```
