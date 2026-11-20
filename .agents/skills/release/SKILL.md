---
name: release
description: Prepares a release of the sisa-caching crates (versions, changelog, semver checks, tag, GitHub release, benchmark table, approval, crates.io) and diagnoses failed release runs. Use when asked about the release PR, the changelog, a version bump or a pre-release, publishing to crates.io, release notes, or a red release workflow.
---

# Release

[release.yml](../../../.github/workflows/release.yml) opens the release PR with release-plz
([release-plz.toml](../../../release-plz.toml)) and does the rest in its own jobs. The five
library crates (`sisa-caching-core`, `-memory`, `-redis`, `-hybrid` and `sisa-caching`) share one
version, one changelog at the root and one `v<version>` tag. `sisa-caching-testing`, the examples
and the benchmarks are never published.

- Releases are cut by hand, so each one batches several changes. The maintainer opens the release
  PR by running the release workflow from the Actions tab (`gh workflow run release.yml`). It
  opens if any crate changed since the last release. The run's `version` input sets the version
  by hand instead of release-plz's pick, as for a pre-release (below).
- Nothing updates the release PR on its own. Running the workflow again brings it up to date
  with `main`: release-plz rewrites a PR that holds only bot commits, and closes one with
  anybody else's commit and opens a new one. Either way, edits are gone.
- The release PR bumps the versions, runs cargo-semver-checks
  against the last published version, and adds a changelog section. release-plz opens it when
  any file a crate packages changed (`cargo package --list`), README included, so a PR whose
  commits are all `docs`, `ci` and the like has a version bump and no changelog entries: write
  its section by hand, or the GitHub release has no notes.
- Merging the release PR changes the version in `Cargo.toml` or the changelog on `main`, which
  starts the release workflow: it tags `v<version>`, creates the GitHub release as a draft from
  the changelog section, and runs the benchmarks, whose table and report go on the draft. A
  version without a `## [<version>]` section in the changelog is not tagged. The publish job then
  waits for the CI run on that commit to pass and for the maintainer to approve the `release`
  environment. Once approved, it publishes the crates to crates.io, and the draft is published. Releases are immutable once published, so nothing may
  change one after that.
- A merged issue is closed while its milestone is still open: the milestone stays open until the
  release is published. The maintainer then closes it by marking it `state: closed` in
  [.github/milestones.yml](../../../.github/milestones.yml).

The maintainer starts and merges the release PR and approves the publish. An agent never starts
or merges it, approves, publishes, or pushes a tag.

## Preparing the release PR

- [ ] Read its changelog section and rewrite it for users: Added, Changed, Fixed, one line
      each, no internal commits. release-plz sorts commits by type (`feat` to Added, `fix` to
      Fixed, `perf` to Changed) and leaves out `refactor`, `docs`, `test`, `build`, `ci` and
      `chore`; a commit without a type lands under Other.
- [ ] If semver-checks reports a breaking change, check the bump matches (before 1.0, a breaking
      change bumps the minor version) and that the changelog says how to migrate.
- [ ] Push the changelog edit to the release PR's branch, then tell the maintainer it is ready,
      with the version and a one-line summary.

## Pre-releases

A pre-release lets a pilot depend on the next version before it is stable: `0.4.0-rc.1`,
`0.4.0-rc.2` and so on, then `0.4.0`, each tagged `v<version>`.

- The maintainer cuts the first rc with `gh workflow run release.yml -f version=0.4.0-rc.1`.
  release-plz opens the release PR with its own pick, and the next step sets the version with
  `release-plz set-version`, which renames the changelog section release-plz wrote; the PR's
  title and body follow.
- Merging releases it like any version. The GitHub release is marked Pre-release, so it is never
  Latest, and its benchmarks compare with the last stable release. On crates.io, `cargo add` and
  requirements such as `"0.4"` skip it: users opt in with `sisa-caching = "0.4.0-rc.1"`, which
  `cargo update` later moves to the next rc and to `0.4.0`.
- The next rc needs no input: from a published `0.4.0-rc.1`, release-plz picks `0.4.0-rc.2`
  whatever the commits are.
- The final needs it too: `-f version=0.4.0`. release-plz lists only
  the commits since the last rc, so write one section for the whole cycle: merge the rc sections
  into the final's, each entry once, link it from the last stable tag, and delete the rc
  sections. Each rc's GitHub pre-release keeps its notes. When nothing in the crates changed
  since the last rc, release-plz opens no PR: open a normal PR with the version and that
  section, titled `chore: release v0.4.0`.
- The milestone stays open until the final is published.

## Gotchas

- Running the workflow again while the release PR is open drops your edits, and the version you
  set unless you pass it again. With a commit of yours on it, release-plz closes it and opens a
  new PR.
- The release workflow runs from its copy on `main`, so a change to it can't be tried in a PR.
- If merging the release PR starts no workflow run, GitHub dropped the push. Push a changelog
  fix to `main`: that tags and drafts the version, since its tag doesn't exist yet. Running the
  workflow by hand doesn't help: a manual run only opens release PRs.
- A publish that stopped halfway is finished by re-running the publish job: it skips the crates
  crates.io already has.
- crates.io limits how fast an account publishes: 5 crates it has never seen, then one more every
  10 minutes; and 30 new versions of crates it has, then one more a minute. The publish job
  waits out a `429 Too Many Requests` and publishes again, for up to an hour in all, so the first
  release of more than 5 new crates takes 10 minutes more for each crate past the fifth. If it
  runs out of time, re-run the failed jobs once the time in the error has passed.
- The first release PR lists every commit since the start. Replace it with the summary in
  [CHANGELOG.md](../../../CHANGELOG.md).

## When the workflow fails

| Error in the log | Fix |
| --- | --- |
| `GitHub Actions is not permitted to create or approve pull requests` | The maintainer allows it in Settings → Actions → General → Workflow permissions |
| `Cannot upload assets to an immutable release` | Something wrote to a published release; record on the draft before `publish release` runs |
| `release-plz opened no release PR` | No crate changed since the last release. For a final right after its last rc, open a normal PR with the version and the cycle's changelog section |
| `wait for ci` fails | The ci run on the tagged commit failed or was cancelled. Fix it on `main`; the fix ships as the next version, or an admin deletes the tag and the draft so the fixed commit is tagged |
| crates.io `403`, or no token from `crates-io-auth-action` | With `CARGO_REGISTRY_TOKEN` set, the token has expired or lacks publish rights on the crate. Without it, the crate needs a trusted publisher for `release.yml` and the `release` environment ([.github/scripts/README.md](../../../.github/scripts/README.md#releases)) |
| `crate ... already exists` | crates.io has that version; re-run the publish job, which skips it |
| `429 Too Many Requests` and `crates.io asks to wait ... past the 3600 s this job waits` | The rate limit outlasted the job's hour of waiting. Re-run the failed jobs after the time the error gives; the crates already out are skipped |

For anything else, find the first error in the failed step's log and report it with a fix.
