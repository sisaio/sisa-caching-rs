# ADR-0003: Redis expiry is relative

- Status: **Accepted**
- Date: 2026-11-20, when it was recorded here; settled earlier, in the architecture page's Decisions table
- Deciders: Sang Au

## Context

A Redis entry expires on the server. The writer can give the server either the time left or an
absolute deadline, and writers in other languages follow the same format.

## Decision

A write sets the expiry with `PEXPIRE` and the time left, in the same transaction as the hash. A
read takes the expiry from `PTTL`, so it comes from Redis, not from the writer's clock.

## Consequences

- Readers and writers never compare clocks.
- The expiry is off only by how long the write takes to arrive: a write held up on its way to the
  server gives its entry that much longer
  ([Guarantees and limits](../architecture.md#guarantees-and-limits)).

## Alternatives considered

- **An absolute deadline with `PEXPIREAT`:** needs the writer's clock to agree with the
  server's.
