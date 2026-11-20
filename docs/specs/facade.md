# Crate: sisa-caching

Status: **Implemented** in 0.1.0 · Published: yes · Label: `area:facade` · Commit scope: `facade`

## Purpose

The crate applications depend on. It re-exports what an application uses from every store crate
and has no code of its own, so each part is also usable as its own crate.

## Public contract

Everything an application uses from [core](core.md), and each store behind its feature. No
feature is on by default: an application names the stores it uses, so with only `memory` or
`redis` on it uses that store crate without the hybrid one.

| Feature | Adds |
| --- | --- |
| `memory` | `InMemoryCacheStore`, the [in-memory store](memory.md) |
| `redis` | `RedisCacheStore`, the [Redis store](redis.md), and the `redis` crate it is built on |
| `hybrid` | `HybridCacheStore`, the [hybrid store](hybrid.md); turns on `memory` and `redis` |
| `tls-rustls` | `rediss://` URLs through rustls; turns on `redis` |
| `tls-native-tls` | `rediss://` URLs through the system's TLS library; turns on `redis` |

## Behaviour

None of its own. CI builds the crate alone with each feature, so one feature never needs
another it doesn't turn on.

## Guarantees and limits

The library's guarantees are in [Architecture](../architecture.md#guarantees-and-limits), and
what each failure does in [Failures](../architecture.md#failures). The redis-rs
types that reach the API through the re-exports are part of the public API: moving to a release
of redis-rs that breaks its own API is breaking here too.

## Errors

None of its own: it re-exports the [core](core.md#errors), [Redis](redis.md#errors) and
[hybrid](hybrid.md#errors) error types.

## Formats

None of its own: see [Formats](../formats.md).
