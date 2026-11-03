# Changelog

All notable changes are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.4.0-rc.1] - 2026-11-03

No changes since the last release.

## [0.3.1](https://github.com/sisaio/sisa-caching-rs/compare/sisa-caching-core-v0.3.0...sisa-caching-core-v0.3.1) - 2026-11-02

### Changed

- *(memory)* size the trim's lists once instead of map by map

## [0.3.0](https://github.com/sisaio/sisa-caching-rs/compare/sisa-caching-core-v0.2.0...sisa-caching-core-v0.3.0) - 2026-11-01

### Added

- *(hybrid)* tell the other nodes when a hybrid store shuts down
- *(hybrid)* count local clears in sisa.caching.local.clears
- *(hybrid)* [**breaking**] give each hybrid store its own backplane channel

### Changed

- *(core)* lock get_or_set's loads by the key alone
- [**breaking**] read the clock through a type parameter, not dyn
- *(core)* build a CacheKey in one string
- box the slow paths of get_or_set and a hybrid read
- *(memory)* keep entries in 16 maps by default
- *(core)* inline the key prefix check into the stores' walks
- count hits and misses without an SDK call per read
- *(memory)* write over a held key without allocating its key
- *(memory)* pick the map with a multiply instead of SipHash
- *(memory)* read no clock to store an entry that never expires
- *(memory)* let a read copy the use counter instead of bumping it
- *(hybrid)* clone no distributed hit while local reads are off
- *(hybrid)* remove the writer's prefix copies on the blocking pool
- *(hybrid)* hash a key once on a local hit

### Fixed

- *(core)* refresh an entry once when another caller just did
- cap every duration setting at 365 days
- *(core)* serve a failed refresh's value only while the store holds it
- *(hybrid)* free a replaced copy after the promotion's turn
- *(redis)* read a creation time past the platform's clock as the epoch
- *(redis)* name both causes of a JSON error without a position
- keep cached values and keys out of warnings
- *(hybrid)* drop a value a promotion turns down after its turn
- *(hybrid)* finish a shutdown whose caller gives up
- *(hybrid)* wait for and count the writes still running at shutdown
- *(hybrid)* stop the worker only once the stop message is out
- *(hybrid)* let each backplane ping run to its timeout
- *(hybrid)* count a write's invalidation in a shutdown right after it
- *(hybrid)* keep a cancelled prefix removal from hiding its keys
- *(hybrid)* bound the sequence numbers a backplane worker tracks
- *(hybrid)* hide a prefix's local copies until their walk is done

### Refactor

- [**breaking**] count each Redis read once, and drop repeated code
- [**breaking**] rename ttl_secs and make the hybrid's local TTL required
- [**breaking**] flatten the shared settings and give the hybrid its own
- [**breaking**] make the settings structs non_exhaustive
- [**breaking**] seal CacheStore and hide what only the store crates use
- *(core)* [**breaking**] move CacheCodec into the Redis crate
- [**breaking**] give each store's own settings a type the hybrid reuses
- *(hybrid)* [**breaking**] drop the backplane pings and the cluster slot check
- [**breaking**] drop two settings getters that return a public field
- [**breaking**] drop DEFAULT_SCAN_BATCH_SIZE for a default in the settings
- *(hybrid)* [**breaking**] drop DEFAULT_LOCAL_TTL_SECS for a settings default
- *(hybrid)* [**breaking**] keep the backplane message format at version 1
- *(hybrid)* [**breaking**] drop the store name from backplane messages

## [0.2.0](https://github.com/sisaio/sisa-caching-rs/compare/v0.1.1...v0.2.0) - 2026-10-29

### Added

- Hybrid nodes catch invalidations lost while the backplane stays subscribed, such as a publish
  that failed or a writer that stopped before publishing. Each store numbers its invalidations
  and sends a heartbeat every `heartbeat_interval_ms` (1 s); a node clears its local layer when a
  heartbeat shows it missed one, or when a node's heartbeats stop for `heartbeat_timeout_ms`
  (5 s). A node that stops, even cleanly, makes the others clear once.
- `sisa.caching.errors` counts the store calls `get_or_set` couldn't make, by `operation`, and
  `get_or_set` warns once per outage and operation instead of at every failed call.
- `sisa.caching.backplane.unreadable` counts the backplane messages a node couldn't read. Such a
  message now clears the local layer instead of ending the subscription.
- `sisa.caching.invalidations.delivered` counts the subscribers each invalidation reached.
- `HybridCacheStore::start_worker` starts the backplane worker before the first call.

### Changed

- Each store starts and stops its own background task: the memory store purges from its first
  write, and the hybrid store runs its backplane worker from its first call. `WorkerHost`,
  `WorkerService`, `PurgeWorker` and `BackplaneWorker` are gone.
- The hybrid store takes a `RedisBackplane`, and `HybridCacheStore` has one type parameter. The
  `Backplane` and `BackplaneSubscription` traits, `BackplaneMessage`, `Invalidation` and
  `RedisSubscription` are gone.
- `validate` rejects 0 in every setting, and a setting that can be off is off with `None`.
  `shards` is at most 1,024.
- `HybridCacheStore::new` checks the backplane's settings, and `with_worker_settings` checks the
  worker settings and returns a `Result`.
- `CacheError::TypeMismatch`, `CacheError::InvalidSettings` and
  `HybridCacheError::StoreNameMismatch` are `#[non_exhaustive]`.
- `purge` is a method of `InMemoryCacheStore`, and the `PurgeableStore` trait is gone.
  `NodeId::new` and `NodeId::generate` are private.
- A read of a value stored as another type, or of Redis data that doesn't decode, counts as a
  miss.
- Worker logs say `worker started`, `worker stopped` and `worker panicked`, with the worker's name
  in `worker` and the store's in `store`.
- A `get_or_set` hit is about 20% faster: it builds the store's full key only when it locks.

### Fixed

- *(redis)* A `SentinelConnection` keeps its connection while Sentinel names the same primary,
  instead of opening a new one after every failed command.
- *(redis)* `remove_prefix` and `clear` work on a cluster whose nodes report no endpoint, as
  behind a load balancer.
- *(hybrid)* A subscription that falls 8,192 messages behind ends and subscribes again, instead of
  growing the process's memory.
- *(hybrid)* A restart that started a new worker host before stopping the old one no longer
  leaves local reads off.
- *(memory)* Trims, purges and the freeing of what `clear` took run on the blocking pool instead
  of holding an async thread, for about 100 to 300 ms at a million entries.
- *(hybrid)* A prefix invalidation from another node walks the local layer on the blocking pool,
  so the worker keeps receiving meanwhile.

### Migrating from 0.1

- Remove the `WorkerHost`, `PurgeWorker` and `BackplaneWorker` lines. On a hybrid store built with
  `new`, call `with_worker_settings(...)?` where you called `BackplaneWorker::with_settings`. Run
  jobs of your own with `tokio::spawn`, and tokio-util's `CancellationToken` and `TaskTracker`
  for a shutdown that waits.
- Write `HybridCacheStore<RedisCacheStore>`, or `RedisHybridCacheStore`, for
  `HybridCacheStore<RedisCacheStore, RedisBackplane>`. A backplane of your own has no
  replacement.
- Replace a 0 that turned a memory setting off with `None` (`null` in JSON). A config file can no
  longer turn off the hybrid's local `max_ttl_secs`; code can, with `None`.
- Give a `BackplaneWorkerSettings` built field by field `heartbeat_interval_ms` and
  `heartbeat_timeout_ms`, or end it with `..BackplaneWorkerSettings::default()`.
- Add `..` to a pattern that names the fields of `TypeMismatch`, `InvalidSettings` or
  `StoreNameMismatch`.
- Drop the `PurgeableStore` import: `store.purge()` works as before, and
  `store.settings().purge_interval()` replaces `store.purge_interval()`.
- Upgrade every node of a hybrid deployment together: a 0.1 node can't read 0.2's heartbeats, so
  while 0.2 nodes share its channel it keeps dropping its subscription and reads from Redis.

The memory and hybrid benches no longer time key formatting, as 0.1.1's did, so this release's
benchmark table shows their reads and writes 25 to 53% faster than 0.1.1 although the stores didn't
change. With the same bench code, 0.2.0 matches 0.1.1, apart from a `get_or_set` hit, about 20%
faster.

## [0.1.1](https://github.com/sisaio/sisa-caching-rs/compare/v0.1.0...v0.1.1) - 2026-10-27

### Fixed

- *(redis)* resend a command refused with LOADING after a failover
- *(hybrid)* take Garnet's sharded messages on a cluster

## [0.1.0](https://github.com/sisaio/sisa-caching-rs/releases/tag/v0.1.0) - 2026-10-26

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
