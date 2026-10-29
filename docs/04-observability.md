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
| `sisa.caching.misses` | counter | `layer` | Reads that returned no value: nothing there, an expired entry, or a value that doesn't read as the requested type |
| `sisa.caching.load.duration` | histogram, seconds | `outcome`: `ok` or `error` | How long each `get_or_set` loader ran |
| `sisa.caching.errors` | counter | `operation`: `get` or `set` | Store calls inside `get_or_set` that failed: its reads, and the stores of what it loaded |
| `sisa.caching.evictions` | counter | | Entries a full memory store dropped to make room |
| `sisa.caching.invalidations.received` | counter | | Other nodes' invalidations a hybrid store applied |
| `sisa.caching.invalidations.delivered` | counter | | Subscribers the server delivered a hybrid store's invalidations to, as `PUBLISH` reports |
| `sisa.caching.publish.failures` | counter | | Invalidations a hybrid store couldn't publish after 3 tries |
| `sisa.caching.backplane.connected` | gauge | | 1 while a hybrid store's backplane is subscribed, else 0 |
| `sisa.caching.backplane.unreadable` | counter | | Backplane messages a hybrid store couldn't read, each of which cleared its local layer |
| `sisa.caching.local.clears` | counter | `reason`: `gap`, `untracked`, `peer_lost`, `resubscribed` or `unreadable` | Times a hybrid store cleared its local layer because it may have missed an invalidation: a number missing below a heartbeat, numbers it can't track, a node whose heartbeats stopped, a new subscription after a lost one, or a message it couldn't read. A clear another node asked for isn't counted |

`layer` is `memory`, `redis`, or `hybrid` for a hybrid store as a whole. A hybrid store records
all three, so its local hit rate is the `memory` series and its overall hit rate the `hybrid`
series.

A `get_or_set` that has to load reads before and after taking its lock, so it counts two misses.
Count loads with the histogram's count instead.

`errors` counts only the calls `get_or_set` makes, whose failures it handles itself: a failed read
loads, and a failed store returns the loaded value without caching it. A call the application makes
returns its error instead. With the store down, each `get_or_set` counts two `get` errors, one per
read, and a `set` error for storing what its loader returned.

`invalidations.delivered` counts every subscriber of the store's channel, the publishing node's
own included, so with every node listening it grows by the number of nodes for each
invalidation. On a cluster, `SPUBLISH` counts only the subscribers on the node
that took it.

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
node stops, even cleanly, every other node logs that clear `heartbeat_timeout_ms` later, so a
rolling deploy logs it once for each node it replaces. The backplane worker also logs at info when
it subscribes. Each store's worker logs starting and stopping at info, and a panic at error, with
the worker's name in `worker` and the store's in `store`.

Warnings name the store, not the key. A `get_or_set` warning fires inside the `cache get_or_set`
debug span, so with debug on it shows next to the key. An error's text can still quote part of a
value, such as a string that no longer decodes into the requested type.

Every target starts with `sisa_caching`, so one filter covers all crates:

```sh
RUST_LOG=info,sisa_caching=debug
```
