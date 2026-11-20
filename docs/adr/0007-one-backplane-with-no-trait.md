# ADR-0007: One backplane, `RedisBackplane`, with no trait for others

- Status: **Accepted**
- Date: 2026-11-20, when it was recorded here; settled earlier, in the architecture page's Decisions table
- Deciders: Sang Au

## Context

The hybrid store tells other nodes about writes over a backplane. The message type, the
subscription and what the worker expects of them are details that may need to change.

## Decision

There is one backplane, `RedisBackplane`, on Redis pub/sub. No trait lets an application supply
a transport of its own.

## Consequences

- The message type, the subscription and what the worker expects of them stay private and can
  change without a breaking release.
- A hybrid store's invalidations always go through a Redis-family server.
- The message itself is still a documented format other languages can publish
  ([Formats](../formats.md#invalidation-message)).

## Alternatives considered

- **A backplane trait:** would make the message type, the subscription and the worker's
  expectations public API.
