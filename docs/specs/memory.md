# Crate: sisa-caching-memory

Status: **Implemented** in 0.1.0 · Published: yes · Label: `area:memory` · Commit scope: `memory`

## Purpose

The in-process store: sharded maps of `Arc<dyn Any>` values with TTLs, an optional entry limit
with least-recently-used trimming, and the worker that purges expired entries. It is also the
local layer of the [hybrid store](hybrid.md). No `moka`: the bounds and the eviction are our own
code.

## Public contract

Main types: `InMemoryCacheStore`, which implements [the store contract](core.md#the-store-contract),
and its settings, `InMemoryCacheSettings` with `MemorySettings` under `memory`. Clones share the
same entries.

## Behaviour

`InMemoryCacheStore` keeps `shards` hash maps (16 by default), each behind its own lock. An entry
stays for its remaining TTL; a hybrid's local layer keeps it for `local.ttl_secs` at most. Past
`max_entries`, a write starts a trim that drops expired entries, then the least recently used ones
down to 90% of the limit, so the next writes don't trim again; writes made while it ran start none,
so it runs again if they left the store over the limit. Values are freed after the lock is released.
Expired entries read as misses and go when they are overwritten, trimmed, or purged. The store
purges every `purge_interval_secs` on its worker. Inside a tokio runtime the trims, the purges and
the freeing of what `remove_prefix` and `clear` took run on the blocking pool, so a store of a
million entries holds up no async thread; the walks of `remove_prefix` and `purge` called by the
application run on the caller's thread.

[Flows](../flows.md#memory-store-write) draws a write, and the
[purge worker](../flows.md#purge-worker).

## Guarantees and limits

The library's guarantees are in [Architecture](../architecture.md#guarantees-and-limits), and
what each failure does in [Failures](../architecture.md#failures). The
memory store has no outside failures: it fails only on an invalid key or setting, and on a read
as another type. `max_entries` is its one bound; it has no byte-size cap
([ADR-0001](../adr/0001-memory-store-bounded-by-entries.md)).

## Errors

The store returns core's [`CacheError`](core.md#errors) and has no error type of its own. Reading
a key as another type is `CacheError::TypeMismatch`.

## Formats

None: values stay Rust values inside the process and never leave it as bytes. Keys follow
[Keys](../formats.md#keys).
