# ADR-0005: Reads stay on the Redis primary

- Status: **Accepted**
- Date: 2026-11-20, when it was recorded here; settled earlier, in the architecture page's Decisions table
- Deciders: Sang Au

## Context

Replicas lag behind their primary. A hybrid node copies what it reads into its local layer.

## Decision

Every read and write goes to a primary. Point every store and backplane at the primary.

## Consequences

- A read never returns a value older than the last write, so a node can't copy a value that was
  just replaced back into its local layer.
- Replicas add no read capacity to a store.
- Behind Sentinel, a connection keeps reading from the old primary after a failover until its
  next write; [Compatibility](../compatibility.md#sentinel) says what that allows.

## Alternatives considered

- **Reading from replicas:** replica lag would let a node copy a value that was just replaced
  back into its local layer.
