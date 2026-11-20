---
name: write-spec
description: Writes or updates a crate's spec in docs/specs, or a flow in docs/flows.md, from the templates. Use before building a feature that changes what a crate does, when adding a crate, or when a spec no longer matches the code.
---

# Write a spec

A spec says what a crate does, so it can be built, reviewed and tested against. The index and
the rules are in [docs/specs/README.md](../../../docs/specs/README.md). The same skill is in
every sisaio crate repository.

- [ ] **A crate.** `docs/specs/<scope>.md`, named after the crate's commit scope, from
      [docs/templates/crate-spec.md](../../../docs/templates/crate-spec.md): purpose, public
      contract, behaviour, guarantees and limits, errors, formats. A new crate also gets a row
      in the specs index and in the crate map of `docs/architecture.md`.
- [ ] **A flow across crates.** A section of `docs/flows.md`, in the shape of
      [docs/templates/flow-spec.md](../../../docs/templates/flow-spec.md).
- [ ] **A data format** (a header, a name on a server, a table) goes in `docs/formats.md`, and
      the spec links to it. Changing one is breaking.
- [ ] **Status.** `Draft` while it is being written, `Accepted` once the maintainer agrees,
      `Implemented in <version>` when the code ships.
- [ ] **A choice that is hard to reverse** gets an ADR too: use the write-adr skill.

## How to write it

- Plainly: short sentences, one idea each, real names from the code. Give the default of every
  setting you mention.
- Say what the code does, not what is planned: planned work goes in the roadmap, and what is
  still undecided under Open questions.
- Don't repeat a guarantee, a format or a rule that lives in another page; link to it.
- Diagrams are Mermaid. A `sequenceDiagram` starts with `autonumber`, and a process `flowchart`
  puts the order in each step's label (`A[1: ...] --> B{2: ...}`), so a step can be named in a
  review or a bug report. Write `1: `, not `1. `, which Mermaid reads as a list. A state diagram
  and a crate map have no order, so they get no numbers.
- Wrap at 100 columns, and check every relative link and `#anchor` with lychee, as
  [Build and check](../../../CONTRIBUTING.md#build-and-check) says.
