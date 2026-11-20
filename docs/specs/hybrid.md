# Crate: sisa-caching-hybrid

Status: **Implemented** in 0.1.0 · Published: yes · Label: `area:hybrid` · Commit scope: `hybrid`

## Purpose

A local memory store on each node in front of a distributed `CacheStore`, kept coherent across
nodes over Redis pub/sub (`RedisBackplane`) by a worker each store runs.

## Public contract

Main types: `HybridCacheStore` (and `RedisHybridCacheStore`, the one over a Redis store), which
implements [the store contract](core.md#the-store-contract); `RedisBackplane` and `NodeId`; its
settings, `HybridCacheSettings` with `LocalSettings` and `BackplaneWorkerSettings`; and
`HybridCacheError`. `HybridCacheStore::connect` builds every part from a URL for one server;
`HybridCacheStore::new` takes a store and a backplane built for a cluster or Sentinel.

## Behaviour

`HybridCacheStore` puts a memory store (the local layer) in front of any `CacheStore` (usually
Redis), and uses `RedisBackplane` (Redis pub/sub) to tell other nodes about writes.
- **Read:** local layer first; on a miss, the distributed store, then copy the entry into the
  local layer.
- **Write:** evict locally, write the distributed store, evict again, publish an
  invalidation (a key, a prefix, or clear). The steps after the first eviction run in their own
  task, so a cancelled caller can't stop the publish. The call returns after the second eviction,
  or for a prefix once its keys read as misses, as its walk runs on the blocking pool; the
  publish and its retries finish in the background.
- **Generations:** each key hashes to one of 64 counters. An invalidation bumps the counter before
  it evicts, and a read only copies into the local layer if the counter didn't move. An
  invalidation waits for a copy in progress, so a slow read can't put back a value that was just
  invalidated, not even for a moment.
- **Backplane worker:** applies other nodes' invalidations. Local reads are on only while it is
  subscribed; every subscribe and every loss clears the local layer.
- Invalidations touch only local layers and generations, never Redis. Each message carries the
  writer's trace context.
- **Channel:** each store publishes and subscribes on its own pub/sub channel,
  `<store>:backplane`, so a node gets only the messages of the stores it runs. On a cluster,
  sharded pub/sub puts each store's channel on the node that owns its slot.

### Several hybrid stores

One store holds values of every type, so an application usually needs one, such as
`catalog_hybrid_cache` holding categories, groups and products under their own key prefixes. Run
a second store, such as `user_hybrid_cache`, when a set of data needs other settings: another
TTL, another `local.max_entries`, or another `local.ttl_secs` for its local copies.

Each store has its own local layer, generations, worker, subscription connection and channel, so
the stores don't affect each other: a burst of writes to one never fills another's subscription
queue, and a lost subscription clears only its own store's local layer. Each store also sends its
own heartbeats, one per `heartbeat_interval_ms` per node. Create each store once at startup and
share it through clones; building one per request would open a subscription each time. Keep store
names a fixed set, with tenants or other variable parts in the keys: each name keeps its hit and
miss counters for the life of the process.

Store names must differ, since the name prefixes every key and names the channel. Redis delivers
a message to subscribers on every database, so two deployments that share a server, such as
production and staging, need different store names even on separate databases: `prod:catalog`
and `staging:catalog`.

[Flows](../flows.md#hybrid-read) draws a read, a
[write and its invalidation](../flows.md#hybrid-write-and-invalidation), and the
[backplane worker](../flows.md#backplane-worker).

## Guarantees and limits

The library's guarantees are in [Architecture](../architecture.md#guarantees-and-limits), and
what each failure does in [Failures](../architecture.md#failures). Every
row about local copies, invalidations and heartbeats there is this crate's.

## Errors

| Type | Returned for |
| --- | --- |
| `HybridCacheError` | `Cache`, `Distributed` (the distributed store's error), `WriteStopped` (the write task panicked, or the runtime shut down before it finished) |

On top of [what `get_or_set` logs and doesn't return](core.md#errors), the hybrid store logs
these as warnings, because the caller still gets a correct answer:
- a publish failed after three tries, logged at the first failure of a run and counted each
  time;
- the backplane subscription was lost or couldn't be made;
- a backplane message couldn't be read, or an invalidation went missing, which clears the local
  layer.

A local-layer type mismatch in the hybrid store reads through to the distributed store.

## Formats

[Keys](../formats.md#keys), and the [invalidation message](../formats.md#invalidation-message)
on the store's `<store>:backplane` channel.
