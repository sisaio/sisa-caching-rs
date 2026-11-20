# Crate: sisa-caching-redis

Status: **Implemented** in 0.1.0 · Published: yes · Label: `area:redis` · Commit scope: `redis`

## Purpose

The store on a Redis-family server (Redis, Valkey, Dragonfly or Garnet): one hash per key with
JSON in `value`, over any `RedisConnection`: one server, a cluster or Sentinel. Other languages
read and write the same hashes.

## Public contract

Main types: `RedisCacheStore`, which implements [the store contract](core.md#the-store-contract);
`RedisConnection`, `SentinelConnection` and `connection_config`; its settings,
`RedisCacheSettings` with `RedisSettings`; and `RedisCacheError`. The `redis` crate it is built on
is re-exported. `RedisCacheStore::connect` takes a URL for one server; `rediss://` URLs need the
`tls-rustls` or `tls-native-tls` feature.

## Behaviour

`RedisCacheStore` keeps one hash per key, with `value` (JSON), `created_at` and `ttl_ms`. A write is
one transaction: `DEL`, `HSET`, `PEXPIRE` with the remaining TTL. A read is `HMGET` and `PTTL` in
one transaction, so the expiry comes from Redis, not from the writer's clock. It works over any
`RedisConnection`: a `ConnectionManager` (the usual one, which reconnects on its own; with
`connection_config()` a call while the server is down fails at once instead of waiting about 9 s), a
`MultiplexedConnection`, a `ClusterConnection` or a `SentinelConnection`. `remove_prefix` and
`clear` walk keys with `SCAN` and `UNLINK`, on each primary of a cluster. `SCAN MATCH` visits every
key in the database, not only the store's, so their cost grows with the whole database: on one with
10 million keys, a `remove_prefix` takes about 10,000 round trips at the default `scan_batch_size`
of 1,000. Give caches a database or an instance of their own, or raise `scan_batch_size`.

[Flows](../flows.md#redis-store-write-and-read) draws a write and a read.

## Guarantees and limits

The library's guarantees are in [Architecture](../architecture.md#guarantees-and-limits), and
what each failure does in [Failures](../architecture.md#failures).
[Compatibility](../compatibility.md) says what works on each server, with replicas, Cluster,
Sentinel and TLS, and how to set the timeouts. Reads stay on the primary
([ADR-0005](../adr/0005-reads-stay-on-the-primary.md)), and the expiry is relative
([ADR-0003](../adr/0003-relative-redis-expiry.md)).

## Errors

| Type | Returned for |
| --- | --- |
| `RedisCacheError` | `Cache`, `Redis` (connection or command), `Json` (encode or decode) |

## Formats

[Keys](../formats.md#keys) and the [Redis entry](../formats.md#redis-entry).
