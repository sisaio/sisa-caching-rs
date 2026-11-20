#!/usr/bin/env bash
# Syncs the repository's labels and milestones with .github/labels.yml and .github/milestones.yml.
# The repo-settings workflow runs it on each push to main that changes either file. Safe to run
# again: what exists is updated in place, and anything on GitHub that the files don't name is left
# alone, except the labels under `delete`.
#
# Needs yq (mikefarah), jq, and the GitHub CLI with GH_TOKEN allowed to write issues. REPO is
# owner/name; by default, the repository of the current folder.
#
# The same file in every sisaio repository.
set -euo pipefail

REPO=${REPO:-$(gh repo view --json nameWithOwner --jq .nameWithOwner)}
labels_file=.github/labels.yml
milestones_file=.github/milestones.yml

# A label name in a URL path: `type:docs` and `good first issue` both need encoding.
encode() { jq -rn --arg value "$1" '$value | @uri'; }

# --- Labels ---------------------------------------------------------------------------------------
existing=$(gh api --paginate "repos/$REPO/labels?per_page=100" --jq '.[].name')
has_label() { grep -qxF -- "$1" <<< "$existing"; }

# Renames first, so a label keeps its issues and PRs under its new name. A label whose new name
# exists already is left for `delete` to remove, or for the maintainer to merge by hand.
while read -r label; do
  name=$(jq -r .name <<< "$label")
  while read -r old; do
    if [ -z "$old" ] || ! has_label "$old"; then continue; fi
    if has_label "$name"; then
      echo "::warning::label '$old' can't be renamed to '$name', which exists; move its issues by hand"
      continue
    fi
    gh api --method PATCH "repos/$REPO/labels/$(encode "$old")" -f new_name="$name" --silent
    existing=$(awk -v old="$old" -v new="$name" '$0 == old { print new; next } { print }' <<< "$existing")
    echo "label $old renamed to $name"
  done < <(jq -r '.was // empty | if type == "array" then .[] else . end' <<< "$label")
done < <(yq -o=json -I=0 '.labels[]' "$labels_file")

# Then every label is created, or its color and description updated.
while read -r label; do
  name=$(jq -r .name <<< "$label")
  gh label create "$name" --repo "$REPO" --force \
    --color "$(jq -r .color <<< "$label")" --description "$(jq -r .description <<< "$label")" > /dev/null
  echo "label $name synced"
done < <(yq -o=json -I=0 '.labels[]' "$labels_file")

# Last, the labels this repository doesn't use.
while read -r name; do
  if [ -n "$name" ] && has_label "$name"; then
    gh label delete "$name" --repo "$REPO" --yes
    echo "label $name deleted"
  fi
done < <(yq -r '.delete // [] | .[]' "$labels_file")

# --- Milestones -----------------------------------------------------------------------------------
# Matched by title. One that exists keeps its number and its issues.
existing=$(gh api --paginate "repos/$REPO/milestones?state=all&per_page=100" --jq '.[] | "\(.number)\t\(.title)"')
while read -r milestone; do
  title=$(jq -r .title <<< "$milestone")
  due=$(jq -r '.due_on // empty' <<< "$milestone")
  args=(-f description="$(jq -r '.description // ""' <<< "$milestone")"
    -f state="$(jq -r '.state // "open"' <<< "$milestone")")
  if [ -n "$due" ]; then args+=(-f due_on="${due}T00:00:00Z"); fi

  number=$(awk -F'\t' -v title="$title" '$2 == title { print $1 }' <<< "$existing")
  if [ -n "$number" ]; then
    gh api --method PATCH "repos/$REPO/milestones/$number" "${args[@]}" --silent
    echo "milestone $title updated"
  else
    gh api "repos/$REPO/milestones" -f title="$title" "${args[@]}" --silent
    echo "milestone $title created"
  fi
done < <(yq -o=json -I=0 '.[]' "$milestones_file")
