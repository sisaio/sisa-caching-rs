# ADR-0006: Each store starts its own worker, once

- Status: **Accepted**
- Date: 2026-11-20, when it was recorded here; settled earlier, in the architecture page's Decisions table
- Deciders: Sang Au

## Context

The memory store needs a task that purges expired entries, and the hybrid store one that applies
other nodes' invalidations. A hybrid store can't run without its listener, or with two.

## Decision

A store starts its worker through its `Worker`, once, inside a tokio runtime: the memory store
its purge worker on its first write, the hybrid store its backplane worker on its first call, in
`connect` or in `start_worker`. The store's last clone dropping stops it.

## Consequences

- Building a store is all the setup: an application starts and wires nothing.
- A store needs a tokio runtime to run its worker.
- The worker holds the store's entries or state but never its `Worker`
  ([Rules](../architecture.md#rules-that-hold-the-design-together)).

## Alternatives considered

- **Workers the application starts:** a store could then run without its listener, or with
  two.
