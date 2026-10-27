#!/usr/bin/env bash
# Syncs the repository rulesets with the JSON files in rulesets/ next to this script: one for main,
# one for the `v*` release tags. Safe to run again: a ruleset whose name exists is replaced, and
# any other is created. Rulesets made by hand under another name are left alone.
#
# Needs the GitHub CLI signed in as a repository admin:
#   gh auth login
set -euo pipefail

REPO=sisaio/sisa-caching-rs
dir=$(dirname "$0")/rulesets

existing=$(gh api "repos/$REPO/rulesets?includes_parents=false&per_page=100" --jq '.[] | "\(.name)|\(.id)"')
for file in "$dir"/*.json; do
  name=$(jq -er .name "$file")
  id=$(awk -F'|' -v name="$name" '$1 == name { print $2 }' <<<"$existing")
  if [ -n "$id" ]; then
    gh api --method PUT "repos/$REPO/rulesets/$id" --input "$file" --silent
    echo "ruleset $name updated"
  else
    gh api --method POST "repos/$REPO/rulesets" --input "$file" --silent
    echo "ruleset $name created"
  fi
done
