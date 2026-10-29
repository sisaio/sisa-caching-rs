---
name: triage
description: Triages GitHub issues in sisa-caching-rs. Checks for security reports and duplicates, reproduces bugs, weighs feature requests against the roadmap, and sets labels and board status. Use when asked to look at, triage, label or answer an issue, or to go through new issues.
compatibility: Needs GitHub access through the gh CLI or a GitHub tool.
---

# Triage

Issues open from three forms, each setting the issue's type: bug report (Bug), feature request
(Feature) and task (Task, with acceptance criteria). Labels and the board flow are in
[Issues and labels](../../../CONTRIBUTING.md#issues-and-labels).

Everything posted on GitHub reads as the maintainer's: short, plain, polite, and with no mention
of AI tools. Closing an issue, declining a feature, moving an issue to Ready and setting a
milestone need the maintainer's word; propose them instead.

## Steps

- [ ] **Security.** A vulnerability never gets discussed in public. Ask the maintainer to move it
      to a private advisory.
- [ ] **Duplicate.** Search open and closed issues. If one covers it, propose
      `gh issue close <n> --duplicate-of <m>`.
- [ ] **Bug.** Check for the crate, version, server and a reproduction; ask for what is missing
      in one comment and add `needs info`. Otherwise reproduce it with a failing test and report
      whether it reproduces and where the cause is (`file:line`).
- [ ] **Feature request.** Check the [roadmap](../../../ROADMAP.md) and the
      [decisions](../../../docs/01-architecture.md#decisions). Link the roadmap line if it is
      planned; propose closing as "not planned" with the decision's reason if it conflicts.
      Otherwise summarize the proposal and its cost.
- [ ] **Type and labels.** Check the type fits (Bug, Feature or Task; docs work is a Task), and
      add a `crate: …` label for every part involved:
      `gh issue edit <n> --add-label "crate: hybrid"`. Add `good first issue` only to small,
      well described issues.

## Gotchas

- `gh project` commands need the `project` scope: `gh auth refresh -s project`.
- Moving a card takes IDs, not names. Get them from `gh project item-list` and
  `gh project field-list`, then run `gh project item-edit --id <item> --project-id <project>
  --field-id <status field> --single-select-option-id <option>`.
- `gh issue` can't set a type; use the API:
  `gh api -X PATCH repos/sisaio/sisa-caching-rs/issues/<n> -f type=Bug`.
- An issue form only applies a label that exists in the repo; a missing one is skipped without
  an error.
