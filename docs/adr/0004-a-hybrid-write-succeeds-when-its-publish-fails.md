# ADR-0004: A hybrid write returns `Ok` when every publish fails

- Status: **Accepted**
- Date: 2026-11-20, when it was recorded here; settled earlier, in the architecture page's Decisions table
- Deciders: Sang Au

## Context

A hybrid write writes the distributed store, then publishes an invalidation so the other nodes
drop their copies. The publish can fail after the data is written.

## Decision

A write whose three publish tries all fail still returns `Ok`. The failure is logged at the
first failure of a run and counted each time.

## Consequences

- The caller isn't told to retry a write that is already in the distributed store.
- The other nodes clear their local layers once the writer's heartbeats cover the write, about
  a second later by default ([Failures](../architecture.md#failures)).
- An operator sees it in `publish.failures` and the warning, not in an error
  ([Observability](../observability.md)).

## Alternatives considered

- **Returning an error:** the data is written, so the caller would see a failure for a write
  that happened.
