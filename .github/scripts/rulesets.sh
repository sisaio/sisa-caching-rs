#!/usr/bin/env bash
# Syncs what only a repository admin can set: the merge settings, and the rulesets with the JSON
# files in rulesets/ next to this script, one for main and one for the `v*` release tags. Safe to
# run again: a ruleset whose name exists is replaced, and any other is created. Rulesets made by
# hand under another name are left alone.
#
# Needs the GitHub CLI signed in as a repository admin:
#   gh auth login
# REPO is owner/name; by default, the repository of the current folder.
#
# The same file in every sisaio repository.
set -euo pipefail

REPO=${REPO:-$(gh repo view --json nameWithOwner --jq .nameWithOwner)}
dir=$(dirname "$0")/rulesets

# PRs merge by rebase only, as the main ruleset also says, and a merged branch is deleted. Work is
# tracked with labels and milestones, so the repository has no project board and no wiki.
gh api --method PATCH "repos/$REPO" --silent \
  -F allow_rebase_merge=true -F allow_squash_merge=false -F allow_merge_commit=false \
  -F delete_branch_on_merge=true -F has_wiki=false -F has_projects=false
echo "merge settings updated"

existing=$(gh api "repos/$REPO/rulesets?includes_parents=false&per_page=100" --jq '.[] | "\(.name)|\(.id)"')
for file in "$dir"/*.json; do
  name=$(jq -er .name "$file")
  id=$(awk -F'|' -v name="$name" '$1 == name { print $2 }' <<<"$existing")
  # On an error, GitHub's reason is in the response body, so print it.
  if [ -n "$id" ]; then
    response=$(gh api --method PUT "repos/$REPO/rulesets/$id" --input "$file") || { echo "$response" >&2; exit 1; }
    echo "ruleset $name updated"
  else
    response=$(gh api --method POST "repos/$REPO/rulesets" --input "$file") || { echo "$response" >&2; exit 1; }
    echo "ruleset $name created"
  fi
done
