# Repository setup scripts

These scripts hold the GitHub settings that don't live in a workflow: the labels, milestones and
project board, and the rulesets on `main` and the release tags. To change a setting, change the
script or its JSON in a PR, then run the script after the merge to sync GitHub with it.

| Script | Syncs | Needs |
| --- | --- | --- |
| `project.sh` | Labels, milestones, and the `sisa-caching` board: its description and README (from `project-readme.md`), Status options, the Priority, Start date and Target date fields, and the views | `gh auth refresh -s project` |
| `rulesets.sh` | The rulesets in `rulesets/`: `main.json` for the default branch, `release-tags.json` for `v*` tags | Repository admin |

Run them from the repository root on an up-to-date `main`:

```sh
.github/scripts/rulesets.sh
.github/scripts/project.sh
```

## Running again

Both scripts can run any number of times:

- Labels and milestone descriptions are updated in place, and rulesets are replaced by name.
- Project fields and views that exist are skipped, because GitHub's API can't edit or delete a
  view. To change one, delete it on the board and run `project.sh` again.
- The Status options are synced on every run. An option that exists keeps its id, so its items
  keep their status. An option the script doesn't list, such as the default Todo, is removed, and
  its items lose their status.
- Labels, views and rulesets made by hand under other names are left alone.

## The rulesets

- `main`: every change goes through a PR, merged with rebase, with linear history, no force-push
  or deletion, and resolved review threads. The checks listed in `main.json` must pass: `ci`,
  which fails when a job it needs fails and passes when they pass or are skipped, the two CodeQL
  `analyze` jobs, and dependency `review`. A new CI job goes in `ci`'s `needs`, not in the list;
  when a listed check is renamed, update the list too, or merges wait on a check that never runs.
- Release PRs from release-plz get no CI, since the built-in token doesn't trigger workflows, so
  repository admins can bypass the `main` rules on a PR (never by a direct push). Merge a release
  PR with "Merge without waiting for requirements".
- `release-tags.json`: only GitHub Actions (the release workflow) and admins can create, move or
  delete a `v*` tag.

## By hand

GitHub has no API for these, so `project.sh` prints them when it finishes:

- Delete the default "View 1", so the board opens on Board, and order the tabs.
- On the Roadmap view, set the date fields to Start date and Target date.
- Turn on the project workflows: auto-add, and the Status changes on add, reopen, linked PR,
  close and merge.

## Security

The scripts hold no secrets. They run under your own `gh` login and no workflow runs them, so no
token with admin or project rights is stored in the repository's secrets.
