# Changelog

All notable changes are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.0](https://github.com/sisaio/sisa-caching-rs/releases/tag/v0.1.0) - 2026-10-26

### Added

- initialize project

### Other

- Initial commit

### Added

- `sisa-caching`: one crate for applications, with features `memory`, `redis` and `hybrid` that
  pick the stores (none on by default), and every type an application uses at
  its root.
- `sisa-caching-memory`: an in-process store with TTLs, an entry limit with LRU eviction, shards,
  and a purge worker.
- `sisa-caching-redis`: a store on Redis, Valkey, Dragonfly or Garnet, writing a hash format other
  languages can share.
- Redis Cluster and Sentinel: `remove_prefix` and `clear` scan every primary of a cluster, and a
  `SentinelConnection`, shared by the store and the backplane, follows a failover.
- `RedisBackplane::cluster`, which carries invalidations over sharded pub/sub on a cluster, as
  Dragonfly's cluster mode needs.
- Dragonfly and Garnet: the whole test suite passes on both, Sentinel aside on Garnet.
- `sisa-caching-hybrid`: a memory layer in front of a distributed store, kept coherent across
  nodes by pub/sub invalidation, with a backplane worker.
- `sisa-caching-core`: string keys and store names with whole-level prefix removal, entries,
  settings, the `CacheStore` trait, `get_or_set` with single-flight loading and refresh-ahead,
  and the worker host.
- `connect` from a URL for the Redis store, the backplane and the hybrid store, with
  `HybridCacheSettings` as one config section for a hybrid deployment.
- `connection_config()`, the settings of every Redis connection the crates build: a call while
  the server is down fails at once instead of waiting about 9 s through redis-rs's reconnect
  attempts.
- `tls-rustls` and `tls-native-tls` features for `rediss://` (neither on by default), with
  `RedisCacheStore::from_client` and `RedisBackplane::from_client` for a client built with a
  private CA or a client certificate.
- Nested settings that reject unknown fields, and `validate` on each settings type, which rejects
  out-of-range values such as a TTL over 365 days, a refresh threshold outside 0 to 1, a ping
  interval of 0, a default TTL or lock wait of 0, or a hybrid `local.common` that nothing would
  read.
- Safe defaults: a 30-second wait for another loader's lock, a purge every 60 seconds, and a
  60-second cap on how long a hybrid node keeps a local copy.
- Tracing spans and OpenTelemetry metrics, with trace context carried across the backplane. Cache
  keys appear only at the debug level, never in info spans or warnings.
