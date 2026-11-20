# Specs

What each crate does, written so it can be built, reviewed and tested against. One page per
crate, all with the same sections: purpose, public contract, behaviour, guarantees and limits,
errors, and formats.

| Crate | Spec | Holds |
| --- | --- | --- |
| `sisa-caching` | [facade.md](facade.md) | The re-exports and the features that pick the stores |
| `sisa-caching-core` | [core.md](core.md) | The store contract: the `CacheStore` trait, keys, entries, settings, `get_or_set` |
| `sisa-caching-memory` | [memory.md](memory.md) | The in-memory store |
| `sisa-caching-redis` | [redis.md](redis.md) | The store on a Redis-family server |
| `sisa-caching-hybrid` | [hybrid.md](hybrid.md) | The hybrid store and its backplane |
| `sisa-caching-testing` | [testing.md](testing.md) | The test servers and helpers; not published |

What spans the crates is one level up: how they fit, the guarantees and the failures in
[Architecture](../architecture.md), the keys, the Redis entry and the invalidation message in
[Formats](../formats.md), and the steps in [Flows](../flows.md).

Changing what a crate does: update its page in the same PR. A new crate starts from
[templates/crate-spec.md](../templates/crate-spec.md). A choice that is hard to reverse gets an
[ADR](../adr/README.md). Planned work goes in the [roadmap](../../ROADMAP.md), not here: a spec
says what the code does, and marks what is still open as an open question.
