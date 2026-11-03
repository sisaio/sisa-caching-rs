---
name: review-changes
description: Reviews a diff or pull request in sisa-caching-rs, code and docs alike, against CI's checks, the design rules, the settled decisions and the docs conventions, and reports findings by severity. Use when asked to review, evaluate or check a change, a branch, a PR or a docs page, even if the word "review" isn't used.
---

# Review changes

Review what changed, not the whole crate, unless asked to review the whole repository (below).
Report findings; don't push fixes unless asked.

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
      can overflow, an error swallowed without a log, a key written without its store prefix,
      and the patterns past reviews found (below).
- [ ] Look for what only shows at scale or during an outage: a std lock or a long loop on an
      async thread, a second copy of something meant to run once (such as two workers for one
      store), a queue or buffer with no bound, and how many log lines and metric points each call
      makes while a store is failing.

## Bugs past reviews found

Each was real, passed CI, and now has a test. Look for the same shape anywhere in a change.

- A flag released before a final recheck, with no ordering that makes the other side's writes
  visible: a trim that ended while writes overfilled the store again. Pair the swaps with
  `AcqRel` and recheck after releasing.
- A last-owner check (`Arc::strong_count`) made outside the lock that hands out clones: a per-key
  lock entry that leaked when two claims dropped at once.
- A time read before waiting for a lock, then compared with what was written during the wait: an
  entry served past its expiry.
- Callers that wait on a shared operation and take its success for granted: a Sentinel lookup
  whose waiters got the old primary when it failed. Hand them its error, and let them redo it if
  its owner was cancelled.
- A future dropped halfway (a caller's timeout, a `select!`) that leaves shared state behind: a
  claim, a running write, a shutdown.
- A watermark that vouches only for a run of finished numbers, held back by an earlier item still
  running: a heartbeat that couldn't reveal a lost invalidation until an earlier write ended.
- A span or log line on a helper another path also calls, or an info line per failed call.
- A test that can't fail: a wait for a value the test never set, or a sleep standing in for proof
  that something happened. Break the code the test guards and watch it fail.

## Whole-repository review

- [ ] Start from `main`'s head and note its commit. Read
      [Guarantees and limits](../../../docs/01-architecture.md#guarantees-and-limits),
      [Failures](../../../docs/01-architecture.md#failures) and the rules first, then each store.
- [ ] For each invariant below, check the test still guards it: break the code and see it fail.
      An invariant the docs promise with no test is a finding.
- [ ] Measure before calling something slow, with the
      [benchmarking method](../../../docs/05-benchmarks.md#running-them) of alternating rounds; for a
      change under 5%, compare callgrind instruction counts instead.
- [ ] Write the findings as one document: severity, evidence, and the fix grouped into proposed
      PRs, each needing its own issue.

| Invariant | Test |
| --- | --- |
| A hybrid write evicts this node's copy before it returns, and other nodes' on delivery | `a_write_updates_the_distributed_layer_and_evicts_the_local_copy`, `every_write_evicts_only_the_copies_it_changes_on_other_nodes` |
| A failed, panicking or abandoned write still evicts everywhere | `a_failed_write_still_evicts_on_every_node`, `a_write_that_panics_returns_an_error_and_still_reaches_other_nodes`, `a_write_whose_caller_gives_up_after_it_was_applied_still_reaches_other_nodes` |
| A read racing a write or an invalidation copies no old value | `a_read_during_a_write_does_not_copy_the_old_value`, `a_read_during_a_removal_does_not_restore_the_value`, `a_promotion_after_an_invalidation_is_dropped` |
| A lost invalidation clears other nodes at the writer's heartbeat, once the writes before it finish | `a_number_missing_below_a_heartbeat_is_a_gap`, `a_publish_refused_on_every_try_clears_the_other_nodes_at_the_next_heartbeat`, `a_lost_invalidation_clears_the_other_nodes_once_the_writes_before_it_finish` |
| A writer that stops or goes silent makes the others clear | `a_node_whose_heartbeats_stop_makes_the_others_clear`, `a_shutdown_that_gives_up_on_a_write_makes_the_other_nodes_clear` |
| A node that calls `shutdown` is forgotten without a clear | `a_node_that_shuts_down_is_forgotten_without_a_clear` |
| A lost subscription turns local reads off until it is back and cleared | `a_lost_subscription_reads_through_until_it_is_back`, `a_closed_subscription_clears_the_copies_and_subscribes_again` |
| A local copy lives at most the local TTL | `local_copies_expire_after_60_seconds_unless_set_otherwise`, `a_read_copies_a_distributed_hit_unchanged_for_the_local_ttl` |
| Deadlines follow the monotonic clock, not the wall clock | `a_wall_clock_change_does_not_move_a_deadline` |
| A memory store full past `max_entries` trims to nine tenths, on the blocking pool | `a_full_store_trims_to_nine_tenths_of_its_limit`, `a_trim_runs_on_the_blocking_pool_while_the_write_goes_on` |
| A per-key lock nobody holds is removed | `a_lock_nobody_holds_is_removed` |
| Calls behind Sentinel fail after the reply timeout while the primary is down | `calls_while_the_primary_is_down_fail_without_queueing_for_lookups`, `callers_that_wait_for_a_failed_lookup_get_its_error` |
| The Redis hash and the backplane message keep their formats | `an_entry_copies_unchanged_between_memory_redis_and_hybrid`, `messages_keep_the_existing_json_format` |

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
