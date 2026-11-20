# Repository setup

The GitHub settings that don't live in a workflow's own file are kept here and in the two files
next to this folder, so a change to one is a PR like any other. A change made only on GitHub
doesn't last: the next sync overwrites it.

| What | Kept in | Synced by |
| --- | --- | --- |
| Labels | [`.github/labels.yml`](../labels.yml) | The [`repo-settings`](../workflows/repo-settings.yml) workflow, on each push to `main` that changes the file; it runs `repo-settings.sh` |
| Milestones | [`.github/milestones.yml`](../milestones.yml) | The same workflow |
| Rulesets on `main` and the `v*` tags | `rulesets/main.json`, `rulesets/release-tags.json` | A repository admin, with `rulesets.sh` |
| Merge settings: rebase only, merged branches deleted, no wiki, no project board | `rulesets.sh` | The same script |

The scripts are the same files in every sisaio repository; only the YAML and JSON they read
differ.

## Labels and milestones

Edit the YAML file in a PR. The workflow syncs GitHub after the merge, with the built-in token,
and can also be run by hand from the Actions tab.

- A label that exists has its color and description updated. To rename one, give the new entry
  `was: "<old name>"`: the label is renamed, so it stays on its issues and PRs. The names under
  `delete` are removed, which takes them off every issue and PR. Any other label on GitHub is
  left alone.
- A milestone is matched by its title and keeps its number and its issues. Its description, due
  date (`due_on`) and state are updated; mark one `state: closed` once its release is published.
  One taken out of the file is left as it is.

There is no project board: an issue's labels, milestone and assignee say where it stands, as
[CONTRIBUTING.md](../../CONTRIBUTING.md#issues-labels-and-milestones) explains.

## Rulesets and merge settings

`GITHUB_TOKEN` can't be given repository administration, so no workflow runs this. After a PR
that changes `rulesets.sh` or the JSON merges, an admin runs it from the repository root on an
up-to-date `main`:

```sh
.github/scripts/rulesets.sh
```

It can run any number of times: the merge settings are set again, a ruleset whose name exists is
replaced, and rulesets made by hand under other names are left alone.

### What the rulesets say

- `main`: every change goes through a PR, merged with rebase, with linear history, no force-push
  or deletion, and resolved review threads. The checks listed in `main.json` must pass: `ci`,
  which fails when a job it needs fails and passes when they pass or are skipped, CodeQL's
  `analyze (actions)`, and dependency `review`. A new CI job goes in `ci`'s `needs`, not in the
  list; when a listed check is renamed, update the list too, or merges wait on a check that never
  runs. CodeQL's Rust scan isn't in the list: it runs on `main` after the merge and every week,
  never on a PR.
- A PR also needs one approval given after its latest push. CodeRabbit's auto-approval gives it;
  the author can't. CODEOWNERS review, two approvers, stale review dismissal and rules for admins
  are left off: they need a second maintainer, or only score with OpenSSF Scorecard once those do.
- A PR's branch doesn't have to be up to date with `main`: with rebase merges, requiring that
  made every merge re-run CI on every other open PR. What lands on `main` is still tested: the CI
  run on the push to `main` is skipped only when the merged tree is the very tree CI passed on in
  the PR.
- Repository admins can still bypass the `main` rules on a PR (never by a direct push): in an
  emergency, when CodeRabbit hasn't approved the latest push, or for a release PR, which
  CodeRabbit doesn't review.
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

crates.io limits how fast an account publishes: 5 crates it has never seen, then one more every
10 minutes; and 30 new versions of crates it has, then one more a minute. The publish job waits
out a `429 Too Many Requests` for up to an hour in all, and skips what is already published when
it is re-run, so a release with many new crates only takes longer. crates.io raises an account's
limit on request, at help@crates.io.

## Security

The scripts hold no secrets. `repo-settings.sh` runs in its workflow with the built-in token,
which can write issues, labels and milestones and nothing else. `rulesets.sh` runs under your own
`gh` login and no workflow runs it, so no token with admin rights is stored in the repository's
secrets.
