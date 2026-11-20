#!/usr/bin/env bash
# Prints `true` when a push to main has the same files as the head of the PR it came from, that
# head already held everything on main, and CI passed on it, so a second run would test nothing
# new. Prints `false` otherwise, on any API error, and when the push changes Cargo.lock or the
# toolchain: only main saves the Rust caches.
#
# A PR run tests the head merged into main as it was then. Only when the head is not behind the
# main it was merged into is that merge the head's own files, which is what the run is trusted for.
# Usage: pr-tested.sh <before sha> <pushed sha>. Needs GH_TOKEN with actions and pull-requests read.
set -euo pipefail

if ! git diff --quiet "$1" "$2" -- Cargo.lock rust-toolchain.toml; then
  echo false
  exit 0
fi

repo="repos/$GITHUB_REPOSITORY"
head=$(gh api "$repo/commits/$2/pulls" --jq '[.[] | select(.merged_at)][0].head.sha // empty') || head=
tree=$(gh api "$repo/git/commits/${head:-none}" --jq .tree.sha 2>/dev/null) || tree=
passed=$(gh api "$repo/actions/workflows/ci.yml/runs?event=pull_request&status=success&head_sha=${head:-none}" \
  --jq .total_count 2>/dev/null) || passed=0
behind=$(gh api "$repo/compare/$1...${head:-none}" --jq .behind_by 2>/dev/null) || behind=
echo "PR head ${head:-none}, tree ${tree:-none}, behind main by ${behind:-unknown}, passing runs $passed" >&2

if [ -n "$head" ] && [ "$tree" = "$(git rev-parse "$2^{tree}")" ] && [ "$behind" = 0 ] && [ "$passed" -gt 0 ]; then
  echo true
else
  echo false
fi
