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
- Repository admins can still bypass the `main` rules on a PR (never by a direct push), for an
  emergency only. Release PRs don't need it once the release app below is set up: they run CI
  like any other PR.
- `release-tags.json`: once a `v*` tag exists, only admins can move or delete it. Creating one
  isn't restricted: GitHub rejects the GitHub Actions app as a bypass actor on this repository's
  rulesets, so a creation rule would stop the release workflow from tagging. A published
  release's tag is also locked by release immutability.

## Releases

`release.yml` needs three things no script can set up:

- **The release app**, so release PRs run CI. Create a GitHub App in the `sisaio` organization
  with Contents and Pull requests set to read and write, and no webhook. Install it on this
  repository, then add its ID as the `RELEASE_APP_ID` secret and a private key as
  `RELEASE_APP_PRIVATE_KEY`. Without them, release PRs fall back to the built-in token and get no
  CI. The app plays no part in publishing to crates.io.
- **The `release` environment**, under Settings → Environments, with the maintainer as required
  reviewer and deployments limited to `main`. The publish job waits there until approved. Create
  it before a release runs: a workflow that names a missing environment makes GitHub create it
  with no reviewers, and the publish then goes out unapproved.
- **A way to publish to crates.io.** While the `CARGO_REGISTRY_TOKEN` secret is set, the publish
  job uses it. To switch to trusted publishing:
  1. On crates.io, open each published crate's Settings → Trusted Publishing → Add, and enter
     owner `sisaio`, repository `sisa-caching-rs`, workflow `release.yml` and environment
     `release`.
  2. Delete every `CARGO_REGISTRY_TOKEN` secret the workflow can see: in the organization, the
     repository and the environment. While one is left, the job keeps using it.
  3. Revoke those tokens on crates.io, under Account Settings → API Tokens.

  The next release then publishes with a short-lived token from `crates-io-auth-action`. A crate
  that isn't on crates.io yet needs one publish with a token before it can have a trusted
  publisher.

## By hand

GitHub has no API for these, so `project.sh` prints them when it finishes:

- Delete the default "View 1", so the board opens on Board, and order the tabs.
- On the Roadmap view, set the date fields to Start date and Target date.
- Turn on the project workflows: auto-add, and the Status changes on add and reopen (issues
  only), linked PR, close and merge. Auto-add doesn't reach back, so add older items by hand.
- Close a milestone once its release is published. The script sets milestone descriptions and
  due dates but never opens or closes one.

## Security

The scripts hold no secrets. They run under your own `gh` login and no workflow runs them, so no
token with admin or project rights is stored in the repository's secrets.
