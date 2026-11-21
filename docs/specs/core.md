# Crate: sisa-caching-core

Status: **Implemented** in 0.1.0 · Published: yes · Label: `area:core` · Commit scope: `core`

## Purpose

The shared parts every store builds on: the `CacheStore` trait, keys, entries, settings with
their 365-day limit, the per-key `Locker` behind `get_or_set`, the `Clock`, the OpenTelemetry
instruments in `metrics`, and the `Worker` that runs a store's background task. No store lives
here, and no Redis client.

## Public contract

Main types: `CacheStore`, `KeyPrefix`, `CacheKey`, `CacheEntry`, `CacheOptions` and
`CacheSettings`. `CacheStore` is sealed: the memory, Redis and hybrid stores are its only
implementations, and code generic over it takes any of them.

### The store contract

`CacheStore` in core is the whole interface:

| Method | What it does |
| --- | --- |
| `get_entry::<CV>(key)` | The one read path: the live entry with its creation time and TTL, or `None` |
| `set_entry(key, entry)` | The one write path: stores the entry until its deadline |
| `get`, `set` | Wrappers over the two paths that take and return the bare value |
| `get_or_set(key, loader)` | Reads, else loads once per key across the store and its clones, and stores the result |
| `remove`, `remove_prefix`, `clear` | Drop one key, every key under a prefix, or the whole store |

Rules every store follows:
- **Entries keep their deadline.** An entry carries the time it was written and its TTL. Stores
  never reset them, so copying an entry from Redis into memory, or writing one from another
  language, keeps the original expiry.
- **Keys.** A store has a name, such as `app`, checked when the store is built, and keys,
  which are any `&str`, such as `user:42`. The stored key is `app::user:42`. A store name never
  holds `::`, so the first `::` always ends it and stores `a` and `a:b` never share keys,
  whatever the key holds. `remove_prefix("user")` removes whole levels: `user:42`, not
  `users:42`, nor the key `user` itself. `CacheKey` is an optional, checked way to build a key.
- **Values.** Any `CV: Serialize + DeserializeOwned + Clone + Send + Sync + 'static`. Memory keeps
  the Rust value itself as `Arc<dyn Any>`, so reading it as another type is
  `CacheError::TypeMismatch`. Redis keeps JSON, so reading it as another type is a decode error.
- **TTLs.** Settings and per-call TTLs are capped at 365 days, checked once in each settings
  type's `validate` and in `CacheOptions::validate`.
- **`set` and `get_or_set`** are default methods of `CacheStore`, written once on top of each
  store's `get_entry` and `set_entry`. `get_or_set` takes a per-key lock, waits up to
  `lock_timeout_ms`, reads again under the lock, and only then loads. Past
  `refresh_threshold`, the first caller to take the lock reads again too, and reloads unless
  another caller has just refreshed the entry; the others get the cached value. A caller whose
  read failed, while the store's last write inside `get_or_set` failed too, gives the lock back
  and loads without it: nothing it loads would reach the callers behind it, so they don't wait
  for it. A read that fails alone, as on a value of another type, keeps the lock.

## Behaviour

The crate opens no connection and starts no worker by itself: a store starts its own through the
`Worker` here. `get_or_set` is written once, in the trait, as
[the store contract](#the-store-contract) says; [Flows](../flows.md#get_or_set-on-every-store)
draws its steps. The `memory` and `redis` crates don't depend on OpenTelemetry directly; they
count through `metrics` here, which [Observability](../observability.md) lists.

## Guarantees and limits

The library's guarantees are in [Architecture](../architecture.md#guarantees-and-limits), and
what each failure does in [Failures](../architecture.md#failures). This
crate adds none of its own.

## Errors

| Type | Returned for |
| --- | --- |
| `CacheError` | `InvalidKey`, `TypeMismatch` (memory), `InvalidSettings` (a setting out of range, such as a TTL over 365 days) |

The error enums and `metrics::Layer` are `#[non_exhaustive]`, so a later release can add a
variant without breaking a `match`. So are the settings structs, so it can add a setting: build
them from `default()` and set fields on it.

Some failures are logged as warnings and not returned, because the caller still gets a correct
answer:
- `get_or_set` couldn't read what the key holds (it stores over it) or couldn't store what it
  loaded, logged at the first failure of a run and counted each time;
- `get_or_set`'s lock wait timed out;
- a refresh failed.

The [hybrid store](hybrid.md#errors) adds its own.

## Formats

[Keys](../formats.md#keys): the store name, the key and the prefix.
