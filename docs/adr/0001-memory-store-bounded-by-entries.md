# ADR-0001: The memory store is bounded by entries, not bytes

- Status: **Accepted**
- Date: 2026-11-20, when it was recorded here; settled earlier, in the architecture page's Decisions table
- Deciders: Sang Au

## Context

The memory store keeps Rust values as `Arc<dyn Any>`, of any type an application caches, and
needs a bound so it can't grow without limit. The size in bytes of an arbitrary Rust value can't
be measured cheaply or reliably.

## Decision

`max_entries` is the one bound on the memory store. There is no byte-size cap.

## Consequences

- Past `max_entries`, a write starts a trim that drops expired entries, then the least recently
  used ones, as the [memory spec](../specs/memory.md#behaviour) says.
- An application sizes the store by counting entries: it has to know roughly how large its
  values are.
- No value is measured or serialized on a write to enforce the bound.

## Alternatives considered

- **A cap in bytes:** needs the size of each value, which can't be measured cheaply or reliably
  for an arbitrary Rust value.
