# sisa-caching docs

sisa-caching gives a Rust service one caching API over three stores. Pick a store when the
service starts; the code that reads and writes the cache stays the same. Add the `sisa-caching`
crate with the store's feature; none is on by default.

| Store | Feature | Where values live | Use it when |
| --- | --- | --- | --- |
| `InMemoryCacheStore` | `memory` | In the process | One process, or data that can differ per process |
| `RedisCacheStore` | `redis` | A Redis-family server | Several processes, or other languages, share the data |
| `HybridCacheStore` | `hybrid` | In each process, in front of Redis | Several processes share the data and reads must be local-fast |

## Layout

```text
docs/
├── README.md          this page: the index and the words used everywhere
├── architecture.md    how the crates fit: goals, crate map, guarantees, failures, rules
├── formats.md         the keys, the Redis entry and the invalidation message other languages share
├── flows.md           diagrams of `get_or_set`, each store's reads and writes, and the workers
├── compatibility.md   Rust and runtime, and what works on each server
├── observability.md   the metrics, every span and log, and what to alert on
├── benchmarks.md      what each benchmark measures and how to run them
├── specs/             what each crate does: one page per crate
├── adr/               architecture decision records: why it is this way
└── templates/         the crate spec and the flow; the ADR template is in adr/
```

At the repository root: [ROADMAP.md](../ROADMAP.md) for what comes next,
[CHANGELOG.md](../CHANGELOG.md) for what shipped, [CONTRIBUTING.md](../CONTRIBUTING.md) for the
checks, code style and Git rules, [SECURITY.md](../SECURITY.md), and [AGENTS.md](../AGENTS.md) for
coding agents. What each type does for users is in its Rust docs, not here: to use a store, its
API docs on docs.rs come with examples.

## Where to start

| You want to | Read |
| --- | --- |
| See how the parts fit, what the library promises and what each failure does | [architecture.md](architecture.md) |
| Change or use one crate | Its page in [specs/](specs/README.md) |
| Read or write the same Redis keys and invalidations from another language | [formats.md](formats.md) |
| Follow a `get_or_set`, a read, a write or a worker step by step | [flows.md](flows.md) |
| Know what works on Redis, Valkey, Dragonfly and Garnet, with replicas, Cluster, Sentinel and TLS | [compatibility.md](compatibility.md) |
| Build a dashboard or an alert | [observability.md](observability.md) |
| Measure a change, or read a release's numbers | [benchmarks.md](benchmarks.md) |
| Know why something was chosen, before proposing another way | [adr/](adr/README.md) |
| Know what's next | [ROADMAP.md](../ROADMAP.md) |
| Contribute, commit, release | [CONTRIBUTING.md](../CONTRIBUTING.md) |

## Words used everywhere

- **Store name**: a string such as `app`, checked when the store is built, that keeps one
  store's keys apart from another's.
- **Key**: any string, such as `user:v1:42`, with levels separated by `:`. On a server the full
  key is `app::user:v1:42`. `CacheKey::new("user:v1", 42)` builds a checked one. The `v1` is the
  version of what the key holds; see [Keys](formats.md#keys).
- **Entry**: a `CacheEntry`, the value with its creation time and TTL. Copying an entry between
  stores keeps its deadline.
- **Settings**: a default TTL (`default_ttl_secs`), when to refresh a value ahead of expiry
  (`refresh_threshold`), and how long `get_or_set` waits for another caller's load
  (`lock_timeout_ms`), at the top level of each store's settings. The memory store adds an entry
  limit (`max_entries`), and a hybrid store how long a node keeps a local copy
  (`local.ttl_secs`), which can't be turned off.
- **Worker**: a background task a store runs for itself. The memory store's purge worker drops
  expired entries; the hybrid store's backplane worker applies other nodes' invalidations.
- **Backplane** and **invalidation**: after a hybrid write, the writing node publishes an
  invalidation over Redis pub/sub, the backplane, and every other node drops its local copy.

## How docs grow

Docs change in the same PR as the code they describe.

1. **A change to what a crate does:** update its page in [specs/](specs/README.md), and the
   diagram in [flows.md](flows.md) if a step changes.
2. **A change to a key, the Redis entry or the invalidation message:** update
   [formats.md](formats.md). It is part of the public API, so the change is breaking.
3. **A new crate:** copy [templates/crate-spec.md](templates/crate-spec.md) to
   `specs/<crate>.md`, and add it to the [specs index](specs/README.md) and the
   [crate map](architecture.md#crates).
4. **A new flow across crates:** add a section to [flows.md](flows.md), in the shape of
   [templates/flow-spec.md](templates/flow-spec.md).
5. **A choice that is hard to reverse, or that changes a rule or a dependency:** write an
   [ADR](adr/README.md). An accepted ADR is never rewritten; a new one supersedes it.

A spec's `Status` moves from Draft to Accepted to Implemented.
