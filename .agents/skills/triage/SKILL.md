---
name: triage
description: Triages GitHub issues in sisa-caching-rs. Checks for security reports and duplicates, reproduces bugs, weighs feature requests against the roadmap and the decisions, and sets labels and proposes a milestone. Use when asked to look at, triage, label or answer an issue, or to go through new issues.
compatibility: Needs GitHub access through the gh CLI or a GitHub tool.
---

# Triage

Issues open from three forms, each setting the issue's type and its `type:` label: bug report
(Bug, `type:bug`), feature request (Feature, `type:feature`) and task (Task, `type:chore`, with
acceptance criteria). The labels, the milestones and how they show where an issue stands are in
[Issues, labels and milestones](../../../CONTRIBUTING.md#issues-labels-and-milestones). There is
no project board.

Everything posted on GitHub reads as the maintainer's: short, plain, polite, and with no mention
of AI tools. Closing an issue, declining a feature and setting a milestone need the maintainer's
word; propose them instead.

## Steps

- [ ] **Security.** A vulnerability never gets discussed in public. Ask the maintainer to move it
      to a private advisory.
- [ ] **Duplicate.** Search open and closed issues. If one covers it, propose
      `gh issue close <n> --duplicate-of <m>`.
- [ ] **Bug.** Check for the crate, version, server and a reproduction; ask for what is missing
      in one comment and add `needs-info`. Otherwise reproduce it with a failing test and report
      whether it reproduces and where the cause is (`file:line`).
- [ ] **Feature request.** Check the [roadmap](../../../ROADMAP.md) and the
      [decisions](../../../docs/adr/README.md). Link the roadmap line if it is
      planned; propose closing as "not planned" with the decision's reason if it conflicts.
      Otherwise summarize the proposal and its cost.
- [ ] **Type and labels.** Check the type fits (Bug, Feature or Task; docs work is a Task with
      `type:docs`) and that the `type:` label matches it, and add an `area:` label for every
      part involved: `gh issue edit <n> --add-label "area:redis"`. Add `good first issue`
      only to small, well described issues.
- [ ] **Milestone.** Propose the release that should ship it, for the maintainer to set when
      accepting it, and `priority:high` if it should be done next.

## Gotchas

- The labels and milestones come from `.github/labels.yml` and `.github/milestones.yml`. Don't
  create or rename one on GitHub: the next sync wouldn't know it. Propose a change to the file.
- `gh issue` can't set a type; use the API:
  `gh api -X PATCH repos/sisaio/sisa-caching-rs/issues/<n> -f type=Bug`.
- An issue form only applies a label that exists in the repo; a missing one is skipped without
  an error.
