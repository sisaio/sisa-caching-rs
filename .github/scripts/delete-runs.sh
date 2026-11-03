#!/usr/bin/env bash
# Deletes finished workflow runs, with their logs and artifacts: every run of one workflow, every
# run created before a date, or both together. It lists what it would delete and asks first.
# Deleted runs can't be restored.
#
# Usage:
#   delete-runs.sh --workflow runner-trial.yml     # every run of one workflow
#   delete-runs.sh --before 2026-10-20             # every run created before that day
#   delete-runs.sh --workflow ci.yml --before 2026-10-20
#   delete-runs.sh ... --yes                       # without asking
#
# Needs the GitHub CLI signed in with write access to Actions:
#   gh auth login
set -euo pipefail

REPO=sisaio/sisa-caching-rs

workflow=
before=
yes=false
while [ $# -gt 0 ]; do
  case $1 in
    --workflow) workflow=$2; shift 2 ;;
    --before) before=$2; shift 2 ;;
    --yes) yes=true; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
if [ -z "$workflow" ] && [ -z "$before" ]; then
  echo "give --workflow, --before or both" >&2
  exit 2
fi

# A run still in progress or queued can't be deleted, so only finished ones are listed. GitHub
# returns at most 1,000 runs per query; run the script again to delete more.
path="repos/$REPO/actions/runs"
[ -n "$workflow" ] && path="repos/$REPO/actions/workflows/$workflow/runs"
query="status=completed&per_page=100"
[ -n "$before" ] && query="$query&created=%3C$before"
runs=$(gh api --paginate "$path?$query" \
  --jq '.workflow_runs[] | "\(.id)\t\(.created_at)\t\(.name)\t\(.display_title)"')

if [ -z "$runs" ]; then
  echo "no runs to delete"
  exit 0
fi
printf '%s\n' "$runs" | cut -f2-
count=$(printf '%s\n' "$runs" | wc -l)
if [ "$yes" != true ]; then
  read -r -p "Delete these $count runs? [y/N] " answer
  [ "$answer" = y ] || [ "$answer" = Y ] || exit 1
fi

for id in $(printf '%s\n' "$runs" | cut -f1); do
  gh api --method DELETE "repos/$REPO/actions/runs/$id" --silent
done
echo "deleted $count runs"
