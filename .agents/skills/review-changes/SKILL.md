---
name: review-changes
description: Reviews a diff or pull request in sisa-caching-rs, code and docs alike, against CI's checks, the design rules, the settled decisions and the docs conventions, and reports findings by severity. Use when asked to review, evaluate or check a change, a branch, a PR or a docs page, even if the word "review" isn't used.
---

# Review changes

Review what changed, not the whole crate. Report findings; don't push fixes unless asked.

## Workflow

- [ ] Get the diff: the PR's diff, or `git diff main...HEAD` on a branch.
- [ ] Run the checks in [CONTRIBUTING.md](../../../CONTRIBUTING.md#build-and-check). Note each
      failure with its first error.
- [ ] Check the diff against the
      [rules](../../../docs/01-architecture.md#rules-that-hold-the-design-together), the
      [decisions](../../../docs/01-architecture.md#decisions) and the
      [code rules](../../../AGENTS.md#code) in AGENTS.md. The ones most often broken:
  - each store keeps one read path (`get_entry`) and one write path (`set_entry`);
  - only the clock module reads the wall clock;
  - invalidations touch the local layer only, never Redis;
  - no new dependency in a library crate, and no `moka`.
- [ ] Check what goes with the change: a test for each new behavior, doc comments on new public
      items, the `docs/` page it affects (above all
      [Data formats](../../../docs/01-architecture.md#data-formats)), and `breaking` on the PR if
      the public API or a data format changed.
- [ ] Check the docs in the diff: doc comments are short and plain with an example where it
      helps, and never link a Markdown file; `docs/` pages use Mermaid for diagrams and tables
      over long lists; every relative link and `#anchor` resolves.
- [ ] Check each commit subject and the PR title against
      [Commit messages](../../../CONTRIBUTING.md#commit-messages): the type matches the change
      (`feat` only for something users can do now, `!` for a breaking change).
- [ ] Look for bugs: a lock held across `.await` or across a value's drop, TTL arithmetic that
      can overflow, an error swallowed without a log, a key written without its store prefix.
- [ ] Look for what only shows at scale or during an outage: a std lock or a long loop on an
      async thread, a second copy of something meant to run once (such as two workers for one
      store), a queue or buffer with no bound, and how many log lines and metric points each call
      makes while a store is failing.

## Gotchas

- Integer literals default to `i32`: `store.set(&key, 5, None)` stores an `i32`, and a later
  `get::<u64>` from the memory store or the hybrid's local layer fails with `TypeMismatch`.
  Tests and examples need a suffix (`5u64`).
- `cargo +1.89 check` can fail where stable passes: a `get_or_set` loader that borrows generic
  state isn't provably `Send` there. Move a cloned `Arc` into the loader. Read the exit code, not
  a piped tail.
- Not bugs, by design: a loading `get_or_set` counts two misses; a read during a write may copy
  Redis into the local layer (the write's second evict removes it); a malformed Redis hash is the
  writer's bug, so the fix is in the docs, not the reader.

## Report

```markdown
**Blocking**
- `crates/hybrid/src/store/hybrid.rs:120`: what is wrong. Fix: what would fix it.

**Not blocking**
- `file:line`: what is wrong. Fix: what would fix it.

Checks: all passed, or the failing check and its first error.
```

If nothing blocks, say so in one line instead of the Blocking section.
