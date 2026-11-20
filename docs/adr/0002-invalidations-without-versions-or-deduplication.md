# ADR-0002: Invalidations carry no per-entry version and aren't deduplicated

- Status: **Accepted**
- Date: 2026-11-20, when it was recorded here; settled earlier, in the architecture page's Decisions table
- Deciders: Sang Au

## Context

A hybrid node copies what it reads from Redis into its local layer, and drops the copy when
another node's invalidation arrives. A read that overlaps a write could put the old value back.
Versions from different nodes' clocks can't be compared safely. The only source of a duplicate
invalidation is a publish retry.

## Decision

A Redis entry carries no version. A write evicts the local copy before and after it writes, and
each key hashes to one of 64 generation counters: an invalidation bumps the counter before it
evicts, and a read copies into the local layer only if the counter didn't move.

Invalidations aren't deduplicated: a node applies every one it receives.

## Consequences

- The Redis entry stays three fields that any language can write
  ([Formats](../formats.md#redis-entry)).
- A slow read can't put back a value that was just invalidated, as the
  [hybrid spec](../specs/hybrid.md#behaviour) says.
- A retried publish evicts a key twice, which is harmless.
- A read that overlaps a write on another node may still copy the old value; the write's
  invalidation then evicts it ([Guarantees and limits](../architecture.md#guarantees-and-limits)).

## Alternatives considered

- **A version in each Redis entry, compared on read:** versions from different nodes' clocks
  can't be compared safely.
- **Deduplicating invalidations:** the only duplicate is a publish retry, and applying an
  eviction twice is harmless.
