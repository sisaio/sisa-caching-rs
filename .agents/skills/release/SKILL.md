---
name: release
description: Prepares a release of the sisa-caching crates (versions, changelog, semver checks, tag, GitHub release, benchmark table, approval, crates.io) and diagnoses failed release runs. Use when asked about the release PR, the changelog, a version bump or a pre-release, publishing to crates.io, release notes, or a red release workflow.
---

# Release

[release.yml](../../../.github/workflows/release.yml) opens the release PR with release-plz
([release-plz.toml](../../../release-plz.toml)) and does the rest in its own jobs. The five
library crates share one version, one changelog at the root and one `v<version>` tag.

- Releases are cut by hand, so each one batches several changes. The maintainer opens the release
  PR by running the release workflow from the Actions tab (`gh workflow run release.yml`). It
  opens if any crate changed since the last release.
- While the release PR is open, CI on every push to `main` updates it. With none open, a push
  opens none.
- The release PR bumps the versions, runs cargo-semver-checks
  against the last published version, and adds a changelog section.
- Once CI passes on the merge, the release workflow tags `v<version>`, creates the GitHub
  release as a draft from the changelog section, and runs the benchmarks, whose table and report
  go on the draft. A merge whose CI fails is not tagged. The publish job then waits for the
  maintainer to approve the `release` environment. Once approved, it publishes the crates to
  crates.io, and the draft is published. Releases are immutable once published, so nothing may
  change one after that.
- Merged issues show as Done on the board; their milestone stays open until the release is
  published, and then the maintainer closes it.

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

- The first rc and the final are set by hand on the release PR. The maintainer opens it as
  usual; on its branch, `release-plz set-version 0.4.0-rc.1` sets the version, the crates'
  requirements on each other and Cargo.lock, and renames the changelog section release-plz
  wrote. Fix the PR's title to match, and push right before merging: like a changelog edit, a
  push to `main` first replaces the PR with release-plz's own pick.
- Merging releases it like any version. The GitHub release is marked Pre-release, so it is never
  Latest, and its benchmarks compare with the last stable release. On crates.io, `cargo add` and
  requirements such as `"0.4"` skip it: users opt in with `sisa-caching = "0.4.0-rc.1"`, which
  `cargo update` later moves to the next rc and to `0.4.0`.
- The next rc needs no input: from a published `0.4.0-rc.1`, release-plz picks `0.4.0-rc.2`
  whatever the commits are.
- The final is set by hand the same way, `release-plz set-version 0.4.0`. release-plz lists only
  the commits since the last rc, so write one section for the whole cycle: merge the rc sections
  into the final's, each entry once, link it from the last stable tag, and delete the rc
  sections. Each rc's GitHub pre-release keeps its notes. When nothing in the crates changed
  since the last rc, release-plz opens no PR: open a normal PR with the version and that
  section, titled `chore: release v0.4.0`.
- The milestone stays open until the final is published.

## Gotchas

- Edit the release PR right before it is merged. If anything lands on `main` first, release-plz
  closes the edited PR and opens a fresh one without the edits.
- The release workflow runs from its copy on `main`, so a change to it can't be tried in a PR.
- If CI on the merge failed, or GitHub dropped the push, nothing is tagged. Fix it on `main` or
  re-run CI: the next CI run that passes tags and drafts the version, since its tag doesn't exist
  yet. Running the release workflow by hand doesn't help: a manual run only opens release PRs.
- A push while a release waits for approval starts no second release: its version's tag exists.
- A publish that stopped halfway is finished by re-running the publish job: it skips the crates
  crates.io already has.
- The first release PR lists every commit since the start. Replace it with the summary in
  [CHANGELOG.md](../../../CHANGELOG.md).

## When the workflow fails

| Error in the log | Fix |
| --- | --- |
| `GitHub Actions is not permitted to create or approve pull requests` | The maintainer allows it in Settings → Actions → General → Workflow permissions |
| `Cannot upload assets to an immutable release` | Something wrote to a published release; record on the draft before `publish release` runs |
| crates.io `403`, or no token from `crates-io-auth-action` | With `CARGO_REGISTRY_TOKEN` set, the token has expired or lacks publish rights on the crate. Without it, the crate needs a trusted publisher for `release.yml` and the `release` environment ([.github/scripts/README.md](../../../.github/scripts/README.md#releases)) |
| `crate ... already exists` | crates.io has that version; re-run the publish job, which skips it |

For anything else, find the first error in the failed step's log and report it with a fix.
