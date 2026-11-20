---
name: write-adr
description: Records an architecture decision as a numbered ADR in docs/adr and links it from the index. Use when a choice is hard to reverse, changes a hard rule, a data format, a dependency or the crate map, or when asked why something was decided and no record exists.
---

# Write an ADR

The rules are in [docs/adr/README.md](../../../docs/adr/README.md). The same skill is in every
sisaio repository.

- [ ] **Number.** The highest in `docs/adr/` plus one, four digits. The file is
      `NNNN-short-kebab-title.md`, copied from
      [0000-template.md](../../../docs/adr/0000-template.md).
- [ ] **Context.** The forces, as facts. Verify every version, price and limit you cite, and
      write the date you checked it.
- [ ] **Decision.** What we will do, as rules a reviewer can check a diff against.
- [ ] **Consequences.** What gets easier and what gets harder, both.
- [ ] **Alternatives considered.** Each one with why not. Write only the alternatives that were
      weighed; ask the maintainer when you don't know them.
- [ ] **Status.** `Proposed` until the maintainer accepts it, which merging the PR does. Record
      who decided and the date.
- [ ] **Index.** Add a row to `docs/adr/README.md`. If it replaces an ADR, mark the old one
      `Superseded by ADR-NNNN` and change nothing else in it.
- [ ] **The docs it touches**, in the same PR: the crate specs, `docs/architecture.md` (the
      rules and the crate map), `docs/formats.md`, and the hard rules in `AGENTS.md`.

## Gotchas

- An accepted ADR is never rewritten, not even to fix its reasoning. A new one supersedes it.
- A decision small enough to live in a spec doesn't need an ADR: write it there.
