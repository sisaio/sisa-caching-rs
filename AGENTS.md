# Agent instructions

Read [docs/01-architecture.md](docs/01-architecture.md) for how the crates fit together and
[CONTRIBUTING.md](CONTRIBUTING.md) for the checks, tests, code style and conventions. This file
adds what an agent working in this repo needs on top.

## Before you push

Run what CI runs, and check exit codes rather than piped output:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo clippy -p sisa-caching-redis -p sisa-caching-hybrid --all-targets --features tls-native-tls -- -D warnings
for f in "" memory redis hybrid tls-rustls tls-native-tls; do cargo check -p sisa-caching --features "$f"; done
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
cargo +1.89 check --workspace --all-targets --all-features
cargo test --workspace --all-features   # needs Docker for the Redis tests
cargo test -p sisa-caching-redis -p sisa-caching-hybrid --features tls-native-tls --test native_tls
cargo deny check
```

## Git and PRs

- Commits carry the contributor's own name as author and committer. Add no co-author or tool
  trailers, and don't mention AI tools in commits, PR titles, PR descriptions or GitHub comments.
- Write every commit message and PR title as [Commit messages](CONTRIBUTING.md#commit-messages)
  says: `type(scope): summary`. Run `git config core.hooksPath .githooks` before your first
  commit so the hook checks them.
- Branch from `main` as `feat/<topic>`, or use the exact branch name you were given. Rebase onto
  `main` to update a branch; never merge `main` into it.
- Label every issue and PR as [Issues and labels](CONTRIBUTING.md#issues-and-labels) says, and
  follow its board flow.
- Labels, milestones, board fields and views, and rulesets come from
  [.github/scripts](.github/scripts/README.md). Propose a change there instead of making one on
  GitHub.
- Never open a PR without an issue. Work only on an issue in Ready, or one the maintainer named;
  when the maintainer asks for work that has none, open it first with the matching form. Assign
  it and move it to In Progress, and to In Review when the PR opens. The PR says `Closes #<n>`
  and carries the issue's kind and crate labels.
- Fill in the [PR template](.github/pull_request_template.md): What changes gives a Before and an
  After, and Evidence shows the commands you ran and what they printed.
- When asked to show or evaluate a change, present the diff for review before pushing.

## Code

- Keep code minimal, readable, and working; the smallest change that does the job wins.
- Each store has one read path (`get_entry`) and one write path (`set_entry`). Don't add helpers
  around them.
- The maintainer picks public names. Use given names exactly, and propose new ones instead of
  inventing them.
- Don't add dependencies to the library crates without asking. No `moka`: memory bounds and
  eviction are our own code.
- Docs go in Rust doc comments, not in Markdown files linked from them.

## Settled decisions

Don't re-propose these without new evidence:

- No byte-size cap on the memory store; the entry cap `max_entries` is enough.
- No deduplication of invalidation messages.
- No per-entry version in Redis, because clocks skew between nodes.
- A hybrid `set` returns `Ok` when every publish attempt fails; it logs a warning, and
  `max_ttl_secs` bounds how stale peers get.
- Reads stay on the Redis primary, never on replicas, because replica lag would undo invalidations.
  The known exception: behind Sentinel, a connection keeps reading from the old primary after a
  failover until its next write.

## Skills

Step-by-step procedures follow the [Agent Skills](https://agentskills.io/specification) format,
one folder per skill in `.agents/skills/`. `.claude/skills/` links to the same folders, so tools
that look in either place find them. Read the matching one before starting:

| Skill | Use it to |
| --- | --- |
| [review-changes](.agents/skills/review-changes/SKILL.md) | Review a diff, PR or docs page against the checks, rules and decisions |
| [release](.agents/skills/release/SKILL.md) | Prepare the release PR or find why a release failed |
| [triage](.agents/skills/triage/SKILL.md) | Triage new issues: duplicates, reproduction, labels, board |
