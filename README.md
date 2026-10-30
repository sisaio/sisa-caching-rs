# sisa-caching-rs

[![CI](https://github.com/sisaio/sisa-caching-rs/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/sisaio/sisa-caching-rs/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/sisaio/sisa-caching-rs/graph/badge.svg)](https://codecov.io/gh/sisaio/sisa-caching-rs)
[![crates.io](https://img.shields.io/crates/v/sisa-caching.svg)](https://crates.io/crates/sisa-caching)
[![docs.rs](https://img.shields.io/docsrs/sisa-caching)](https://docs.rs/sisa-caching)
[![MSRV](https://img.shields.io/badge/MSRV-1.89-blue)](https://github.com/sisaio/sisa-caching-rs/blob/main/Cargo.toml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/sisaio/sisa-caching-rs/blob/main/LICENSE)

Async caching for Rust with one API over three stores: an in-memory cache, a Redis-family cache,
and a hybrid of the two that keeps every node's local copy coherent.

## Which store

| Feature | Store | Use it when |
| --- | --- | --- |
| `memory` | `InMemoryCacheStore` | One process. Fastest; TTLs, an entry limit with LRU eviction, sharded maps. |
| `redis` | `RedisCacheStore` | Several processes share the cache. Redis, Valkey, Dragonfly or Garnet: one primary with or without replicas, a Redis Cluster, or Sentinel. Other languages can read and write the same keys. |
| `hybrid` | `HybridCacheStore` | Several processes, and reads must be local-fast. A memory layer per node in front of Redis, kept coherent by pub/sub invalidation. |

Everything comes from one crate, `sisa-caching`. No store is on by default, so name the one you use;
`tls-rustls` or `tls-native-tls` adds `rediss://` URLs. Each part is also its own crate
([core](https://github.com/sisaio/sisa-caching-rs/tree/main/crates/core),
[memory](https://github.com/sisaio/sisa-caching-rs/tree/main/crates/memory),
[redis](https://github.com/sisaio/sisa-caching-rs/tree/main/crates/redis),
[hybrid](https://github.com/sisaio/sisa-caching-rs/tree/main/crates/hybrid)), for implementing a
store of your own.

Every store has the same calls: `get`, `set`, `get_or_set`, `remove`, `remove_prefix` and
`clear`, plus `get_entry` and `set_entry` for a value with its creation time and TTL. One store
holds values of every type, named per call, so an application creates one store, and moving to
another store doesn't touch its call sites. The crate docs cover settings and behavior in full.

## Quick start

```toml
[dependencies]
sisa-caching = { version = "0.3", features = ["memory"] }   # or "redis", or "hybrid"
```

In memory:

```rust,ignore
use sisa_caching::{CacheStore, InMemoryCacheStore};

// It drops expired entries every 60 seconds by itself.
let cache = InMemoryCacheStore::new("app")?;

let user: User = cache.get_or_set(&format!("user:v1:{id}"), || load_user(id), None).await?;
cache.remove_prefix("user").await?;
```

On Redis (`rediss://` with the `tls-rustls` or `tls-native-tls` feature):

```rust,ignore
use sisa_caching::RedisCacheStore;

let cache = RedisCacheStore::connect("app", "redis://127.0.0.1:6379").await?;
```

Hybrid, which applies the other nodes' invalidations by itself:

```rust,ignore
use sisa_caching::{HybridCacheSettings, HybridCacheStore};

let settings: HybridCacheSettings = load_config_section("cache")?;
let cache = HybridCacheStore::connect("app", redis_url, &settings).await?;

// At shutdown, after the last write: the other nodes forget this one instead of clearing.
cache.shutdown().await;
```

When an invalidation never reaches a node, the node clears its local layer at the writer's next
heartbeat (1 second by default), or once the writer's heartbeats have stopped for 5 seconds. The
local `max_ttl_secs`, 60 seconds by default, still caps every local copy.

Each hybrid store has its own pub/sub channel, `<store>:backplane`. One store usually covers an
application; a second one, such as `user_cache` next to `catalog_cache`, gets its own settings
and doesn't affect the first. See
[Several hybrid stores](https://github.com/sisaio/sisa-caching-rs/blob/main/docs/01-architecture.md#several-hybrid-stores).

## Compatibility

Tested in CI against Redis 7.4, Redis 8, Valkey 8, Dragonfly 2 and Garnet 2, each as one primary, a
three-primary cluster, and a primary and replica behind Sentinel through a failover, except Sentinel
on Garnet, which has none.
[docs/03-compatibility.md](https://github.com/sisaio/sisa-caching-rs/blob/main/docs/03-compatibility.md)
says how to connect to each.

## Observability

Stores trace through `tracing` and count hits, misses, loads, failed store calls and evictions
through the OpenTelemetry metrics API, under `sisa.caching.*`. A hybrid write and the invalidations
it causes on other nodes share one trace.
[docs/04-observability.md](https://github.com/sisaio/sisa-caching-rs/blob/main/docs/04-observability.md)
lists every span and log line and what to alert on, and the
[metrics docs](https://docs.rs/sisa-caching-core/latest/sisa_caching_core/metrics/) every metric;
the examples show the setup.

## Examples

Three axum services serve the same user API from `examples/users-api`, each on a different
store: `axum-memory`, `axum-redis` and `axum-hybrid`. Each caches a scalar
(`GET /users/count`), an object (`GET /users/{id}`) and a list (`GET /users`). The Redis and
hybrid ones need a server at `REDIS_URL` (default `redis://127.0.0.1:6379`). A fourth,
`axum-config`, runs on whichever store its `CACHE` variable names (`memory`, `redis` or
`hybrid`), for an application that picks its store from configuration.

```sh
cargo run -p axum-memory

# Two hybrid instances: a write on one is seen by the other.
PORT=3000 cargo run -p axum-hybrid
PORT=3001 cargo run -p axum-hybrid
curl localhost:3001/users/1
curl -X PUT localhost:3000/users/1 -H 'content-type: application/json' -d '{"name":"Ada L."}'
curl localhost:3001/users/1
```

They push traces, metrics and logs over OTLP/HTTP to `OTEL_EXPORTER_OTLP_ENDPOINT` (default
`http://localhost:4318`) and still run without a collector. To print what they send:

```sh
docker run --network host otel/opentelemetry-collector \
  --config 'yaml:receivers: {otlp: {protocols: {http: {endpoint: 0.0.0.0:4318}}}}' \
  --config 'yaml:exporters: {debug: {verbosity: detailed}}' \
  --config 'yaml:service: {pipelines: {traces: {receivers: [otlp], exporters: [debug]}, metrics: {receivers: [otlp], exporters: [debug]}, logs: {receivers: [otlp], exporters: [debug]}}}'
```

## Benchmarks

Each release records its benchmarks on its
[release page](https://github.com/sisaio/sisa-caching-rs/releases/latest): reads and writes on
every store, each compared with the previous release on the same machine, and Criterion's full
report.
[docs/05-benchmarks.md](https://github.com/sisaio/sisa-caching-rs/blob/main/docs/05-benchmarks.md) explains what each one measures and how to run them.

## More

- [Docs](https://github.com/sisaio/sisa-caching-rs/blob/main/docs/00-overview.md): an overview, then architecture, flows, compatibility,
  observability and benchmarks, in reading order.
- [Roadmap](https://github.com/sisaio/sisa-caching-rs/blob/main/ROADMAP.md) and [changelog](https://github.com/sisaio/sisa-caching-rs/blob/main/CHANGELOG.md).
- [Contributing](https://github.com/sisaio/sisa-caching-rs/blob/main/CONTRIBUTING.md): building, testing and conventions.
- [Security](https://github.com/sisaio/sisa-caching-rs/blob/main/SECURITY.md): reporting a vulnerability.

Rust 1.89 or newer. MIT licensed.
