# Overview

sisa-caching gives a Rust service one caching API over three stores. Pick a store when the
service starts; the code that reads and writes the cache stays the same. Add the `sisa-caching`
crate with the store's feature; none is on by default.

| Store | Feature | Where values live | Use it when |
| --- | --- | --- | --- |
| `InMemoryCacheStore` | `memory` | In the process | One process, or data that can differ per process |
| `RedisCacheStore` | `redis` | A Redis-family server | Several processes, or other languages, share the data |
| `HybridCacheStore` | `hybrid` | In each process, in front of Redis | Several processes share the data and reads must be local-fast |

## The words used everywhere

- **Store name**: a string such as `app`, checked when the store is built, that keeps one
  store's keys apart from another's.
- **Key**: any string, such as `user:v1:42`, with levels separated by `:`. On a server the full
  key is `app::user:v1:42`. `CacheKey::new("user:v1", 42)` builds a checked one. The `v1` is the
  version of what the key holds; see [Keys](01-architecture.md#keys).
- **Entry**: a `CacheEntry`, the value with its creation time and TTL. Copying an entry between
  stores keeps its deadline.
- **Settings**: a default TTL (`ttl_secs`), when to refresh a value ahead of expiry
  (`refresh_threshold`), and how long `get_or_set` waits for another caller's load
  (`lock_timeout_ms`). The memory store adds an entry limit and a residency cap
  (`max_entries`, `max_ttl_secs`).
- **Worker**: a background task a store runs for itself. The memory store's purge worker drops
  expired entries; the hybrid store's backplane worker applies other nodes' invalidations.
- **Backplane** and **invalidation**: after a hybrid write, the writing node publishes an
  invalidation over Redis pub/sub, the backplane, and every other node drops its local copy.

## Reading order

1. [Architecture](01-architecture.md): the crates, the store contract, what each failure does,
   the Redis and invalidation formats other languages share, and the rules the design depends on.
2. [Flows](02-flows.md): diagrams of `get_or_set`, each store's reads and writes, and the
   workers.
3. [Compatibility](03-compatibility.md): Rust and runtime, what works on Redis, Valkey,
   Dragonfly and Garnet with replicas, Cluster, Sentinel and TLS.
4. [Observability](04-observability.md): the metrics, every span and log, and what to alert on.
5. [Benchmarks](05-benchmarks.md): what each benchmark measures and where each release's numbers
   are.

What comes next is in the [roadmap](../ROADMAP.md). To use a store, its API docs on docs.rs come
with examples. To change the code, read [CONTRIBUTING.md](../CONTRIBUTING.md) after the
architecture.
