# Architecture decision records

Short records of significant decisions: why, not only what. The rules:

1. Copy [0000-template.md](0000-template.md) to the next number, such as `0008-short-title.md`.
2. Open it as **Proposed** in the PR that needs it. It becomes **Accepted** when the PR merges.
3. Never rewrite an accepted ADR. Write a new one and mark the old one **Superseded by
   ADR-XXXX**.
4. Write an ADR when a choice is hard to reverse, changes a rule, a data format or a dependency,
   or someone will ask "why did we do this?" in six months.

Don't re-propose an accepted decision without new evidence.

| # | Decision | Status |
| --- | --- | --- |
| [0001](0001-memory-store-bounded-by-entries.md) | The memory store is bounded by `max_entries`, with no byte-size cap | Accepted |
| [0002](0002-invalidations-without-versions-or-deduplication.md) | No per-entry version in Redis and no deduplication of invalidations; generations and eviction-on-write cover the race | Accepted |
| [0003](0003-relative-redis-expiry.md) | Redis expiry is relative (`PEXPIRE`), not absolute (`PEXPIREAT`) | Accepted |
| [0004](0004-a-hybrid-write-succeeds-when-its-publish-fails.md) | A hybrid write returns `Ok` when every publish of its invalidation fails | Accepted |
| [0005](0005-reads-stay-on-the-primary.md) | Reads stay on the Redis primary, never a replica | Accepted |
| [0006](0006-each-store-starts-its-own-worker.md) | Each store starts its own worker, once; building a store is all the setup | Accepted |
| [0007](0007-one-backplane-with-no-trait.md) | One backplane, `RedisBackplane`, with no trait for others | Accepted |
