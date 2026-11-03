#!/usr/bin/env bash
# Puts a version set by hand on the open release PR, once release-plz has opened or rewritten it.
# release-plz picks the version from the commits and rewrites the PR from scratch on every push to
# main, so a version set by hand is put back each time: the manual run's `version` input, or else
# the version the PR had before release-plz rewrote it, when that one is a pre-release or above
# release-plz's pick. A bump release-plz made itself still follows the commits.
#
# `release-plz set-version` sets the version, the crates' requirements on each other and Cargo.lock,
# and renames the changelog section release-plz just wrote; the PR's title and body follow. The
# change goes into release-plz's own commit, so the PR stays one commit that release-plz still
# counts as its own and rewrites on the next push.
#
# When nothing changed since the last release, as for a final right after its last rc, release-plz
# opens no PR. With a `version` input, the script then opens one itself, on a branch named like
# release-plz's, with the version and a changelog section to fill in. release-plz leaves an open PR
# alone while nothing changes, and rewrites it, with the version put back, once something does.
# Without the input, it does nothing.
#
# Usage: release-version.sh <version input> <the PR's version before release-plz ran>; either may
# be empty. Needs release-plz on PATH and GH_TOKEN that can push to the PR's branch and edit the PR.
set -euo pipefail

wanted=$1
before=$2

# Exits 0 when `before` stays over `picked`: a pre-release always does, and so does any version
# above the pick, such as a final over the next rc. Versions compare by semver precedence.
keeps() {
  python3 - "$1" "$2" <<'EOF'
import sys


def precedence(version):
    core, _, pre = version.partition("-")
    numbers = tuple(int(part) for part in core.split("."))
    if not pre:
        return numbers, (1,)
    parts = tuple((0, int(part), "") if part.isdigit() else (1, 0, part) for part in pre.split("."))
    return numbers, (0, parts)


before, picked = sys.argv[1:]
sys.exit(0 if "-" in before or precedence(before) > precedence(picked) else 1)
EOF
}

# The workspace version in Cargo.toml.
workspace_version() {
  python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml", "rb"))["workspace"]["package"]["version"])'
}

# Adds an empty section for version $1 at the top of the changelog, under `## [Unreleased]`.
add_changelog_section() {
  python3 - "$1" <<'EOF'
import datetime
import sys

heading = "## [Unreleased]\n\n"
section = f"## [{sys.argv[1]}] - {datetime.date.today()}\n\nNo changes since the last release.\n\n"
with open("CHANGELOG.md") as changelog:
    text = changelog.read()
with open("CHANGELOG.md", "w") as changelog:
    changelog.write(text.replace(heading, heading + section, 1))
EOF
}

push() {
  git push --quiet "$@" "https://x-access-token:$GH_TOKEN@github.com/$GITHUB_REPOSITORY.git" \
    "HEAD:refs/heads/$branch"
}

pr=$(gh pr list --repo "$GITHUB_REPOSITORY" --label release --state open --base "$GITHUB_REF_NAME" \
  --json number,headRefName --jq 'first // empty')

if [ -z "$pr" ]; then
  if [ -z "$wanted" ]; then
    echo "no release PR is open"
    exit 0
  fi
  current=$(workspace_version)
  if [ "$wanted" = "$current" ]; then
    echo "::error::$GITHUB_REF_NAME is at $wanted already"
    exit 1
  fi
  branch="release-plz-$(date -u +%Y-%m-%dT%H-%M-%SZ)"
  git switch --quiet --create "$branch"
  release-plz set-version "$wanted"
  # set-version renamed the last release's section; the new version gets a section of its own.
  git checkout --quiet -- CHANGELOG.md
  add_changelog_section "$wanted"
  git -c user.name='github-actions[bot]' \
    -c user.email='41898282+github-actions[bot]@users.noreply.github.com' \
    commit --quiet --all --message "chore: release v$wanted"
  push
  gh pr create --repo "$GITHUB_REPOSITORY" --base "$GITHUB_REF_NAME" --head "$branch" \
    --label release --title "chore: release v$wanted" \
    --body "Releases $wanted. Nothing in the crates changed since $current, so release-plz opened no release PR. Write the changelog section before merging."
  exit 0
fi

number=$(jq -r .number <<<"$pr")
branch=$(jq -r .headRefName <<<"$pr")
git fetch --quiet origin "$branch"
head=$(git rev-parse FETCH_HEAD)
git switch --quiet --force-create "$branch" "$head"
picked=$(workspace_version)

version=$wanted
if [ -z "$version" ] && [ -n "$before" ] && keeps "$before" "$picked"; then
  version=$before
fi
if [ -z "$version" ] || [ "$version" = "$picked" ]; then
  echo "release PR #$number keeps release-plz's version, $picked"
  exit 0
fi

release-plz set-version "$version"
git -c user.name="$(git log -1 --format=%an)" -c user.email="$(git log -1 --format=%ae)" \
  commit --quiet --all --amend --message "chore: release v$version"
push --force-with-lease="refs/heads/$branch:$head"

title=$(gh pr view "$number" --repo "$GITHUB_REPOSITORY" --json title --jq .title)
body=$(gh pr view "$number" --repo "$GITHUB_REPOSITORY" --json body --jq .body)
gh pr edit "$number" --repo "$GITHUB_REPOSITORY" --title "${title//"$picked"/"$version"}" \
  --body-file <(printf '%s\n' "${body//"$picked"/"$version"}")
echo "release PR #$number now releases $version instead of $picked"
