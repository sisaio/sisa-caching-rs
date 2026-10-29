Work on [sisa-caching-rs](https://github.com/sisaio/sisa-caching-rs): async caching for Rust with one API over an in-memory store, a Redis-family store, and a hybrid of the two.

## How work moves

Every PR closes an issue. An issue moves through these columns:

| Status | Meaning |
| --- | --- |
| Backlog | Triaged, not planned yet. |
| Ready | Accepted and in a milestone; anyone can start. |
| In Progress | Someone is assigned and working on it. |
| Blocked | Waiting on something outside the issue; a comment says what. |
| In Review | A pull request with `Closes #n` is open. |
| Done | Merged, or closed as duplicate or not planned. |

Releases are cut by hand, so a Done issue whose milestone is still open hasn't shipped yet. The maintainer closes a milestone once its release is published.

Only the maintainer moves an issue to Ready and sets its milestone and Priority. Assign yourself before you start.

## Fields

- **Priority**: P0 urgent, P1 needed for the current milestone, P2 can slip, P3 nice to have.
- **Milestone**: the release that ships it.
- **Start date** and **Target date**: place the issue on the Roadmap.

## Views

- **Board**: issues by Status.
- **Backlog**: issues waiting for triage.
- **Delivery**: issues per milestone, which is a release, with their status and kind (bug, enhancement, task).
- **Roadmap**: the same issues on a timeline.
- **Blocked**: what is stuck.
- **My work**: your open issues.
- **Pull requests**: open PRs and their reviewers.

The labels and the board flow are described in [CONTRIBUTING.md](https://github.com/sisaio/sisa-caching-rs/blob/main/CONTRIBUTING.md#issues-and-labels). This board is set up by [.github/scripts/project.sh](https://github.com/sisaio/sisa-caching-rs/blob/main/.github/scripts/project.sh), so change it there.
