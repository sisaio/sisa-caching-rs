---
name: release
description: Prepares a release of the sisa-caching crates (versions, changelog, semver checks, tag, GitHub release, benchmark table) with release-plz, and diagnoses failed release runs. Use when asked about the release PR, the changelog, a version bump, publishing to crates.io, release notes, or a red release workflow.
---

# Release

[release.yml](../../../.github/workflows/release.yml) runs release-plz
([release-plz.toml](../../../release-plz.toml)) on every push to `main`. The five library crates
share one version, one changelog at the root and one `v<version>` tag.

- Each push opens or updates one release PR. It bumps the versions, runs cargo-semver-checks
  against the last published version, and adds a changelog section.
- Merging the release PR publishes to crates.io, tags `v<version>`, creates the GitHub release
  as a draft, runs the benchmarks, whose table and report go on the draft, and then publishes
  it. Releases are immutable once published, so nothing may change one after that.

The maintainer merges the release PR. An agent never merges it, publishes, or pushes a tag.

## Preparing the release PR

- [ ] Read its changelog section and rewrite it for users: Added, Changed, Fixed, one line
      each, no internal commits. release-plz sorts commits by type (`feat` to Added, `fix` to
      Fixed, `perf` to Changed) and leaves out `refactor`, `docs`, `test`, `build`, `ci` and
      `chore`; a commit without a type lands under Other.
- [ ] If semver-checks reports a breaking change, check the bump matches (before 1.0, a breaking
      change bumps the minor version) and that the changelog says how to migrate.
- [ ] Push the changelog edit to the release PR's branch, then tell the maintainer it is ready,
      with the version and a one-line summary.

## Gotchas

- Edit the release PR right before it is merged. If anything lands on `main` first, release-plz
  closes the edited PR and opens a fresh one without the edits.
- The first release PR lists every commit since the start. Replace it with the summary in
  [CHANGELOG.md](../../../CHANGELOG.md).
- Never list a `publish = false` crate in `release-plz.toml`. Any `[[package]]` entry turns
  publishing on, and the release job fails with "has `publish = false` ... but it has
  `publish = true` in the release-plz configuration".

## When the workflow fails

| Error in the log | Fix |
| --- | --- |
| `GitHub Actions is not permitted to create or approve pull requests` | The maintainer allows it in Settings → Actions → General → Workflow permissions |
| `has publish = false ... publish = true in the release-plz configuration` | Remove that crate's entry from `release-plz.toml` |
| `Cannot upload assets to an immutable release` | Something wrote to a published release; record on the draft before `publish release` runs |
| crates.io `403` or no token | A crate's first publish needs the `CARGO_REGISTRY_TOKEN` secret; later ones use trusted publishing |

For anything else, find the first error in the failed step's log and report it with a fix.
