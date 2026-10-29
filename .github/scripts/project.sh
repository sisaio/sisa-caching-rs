#!/usr/bin/env bash
# Syncs the labels, milestones and the project board (description, README, Status, fields and
# views) of sisa-caching-rs with this file and project-readme.md. Safe to run again: labels and
# milestone descriptions and due dates are updated in place, and fields and views that exist are
# skipped. Other labels and views made by hand are left alone.
#
# Needs the GitHub CLI signed in with the project scope:
#   gh auth login
#   gh auth refresh -s project
set -euo pipefail

OWNER=sisaio
REPO=sisaio/sisa-caching-rs
TITLE=sisa-caching

# --- Labels ---------------------------------------------------------------------------------------
# name|color|description
labels=(
  "documentation|0075ca|Docs only: doc comments, docs/, README."
  "crate: sisa-caching|1d76db|sisa-caching: the facade crate, its features and re-exports."
  "crate: core|1d76db|sisa-caching-core: traits, entries, settings, metrics."
  "crate: memory|1d76db|sisa-caching-memory: the in-memory store."
  "crate: redis|1d76db|sisa-caching-redis: the Redis-family store."
  "crate: hybrid|1d76db|sisa-caching-hybrid: the hybrid store and backplane."
  "ci|5319e7|Workflows, release, benchmarks and tooling."
  "needs info|fbca04|Waiting on the reporter for details or a reproduction."
  "breaking|b60205|Changes the public API or a data format."
  "good first issue|7057ff|Small and well described; a good start for a new contributor."
  "help wanted|008672|The maintainer would welcome a pull request."
  "dependencies|0366d6|A dependency update, usually from Dependabot."
  "release|c5def5|The release PR that release-plz opens."
)
for line in "${labels[@]}"; do
  IFS='|' read -r name color description <<<"$line"
  gh label create "$name" --repo "$REPO" --color "$color" --description "$description" --force
done

# Labels we don't use: issues close as "duplicate" or "not planned" instead, questions are
# answered in their issue, an issue's type (Bug, Feature, Task) says its kind, and Dependabot's
# PRs get `dependencies` only. Deleting a label removes it from every issue and PR that has it.
for name in duplicate invalid question wontfix bug enhancement task accessibility github_actions rust; do
  gh label delete "$name" --repo "$REPO" --yes 2>/dev/null || true
done

# --- Milestones -----------------------------------------------------------------------------------
# A milestone is a release: it stays open until that release is published, and its due date is the
# target. title|due date (YYYY-MM-DD, or empty for none yet)|description
milestones=(
  "0.1.0||First release on crates.io."
  "0.2.0|2026-10-29|Breaking: stores run their own workers, and hybrid nodes detect lost invalidations."
  "0.3.0|2026-11-01|Picked from the roadmap's Later list, as users ask for it."
)
# Existing milestones keep their number, issues and state; the description and due date are
# updated. Closing a milestone after its release is left to the maintainer.
existing=$(gh api "repos/$REPO/milestones?state=all&per_page=100" --jq '.[] | "\(.title)|\(.number)"')
for line in "${milestones[@]}"; do
  IFS='|' read -r title due description <<<"$line"
  args=(-f description="$description")
  if [ -n "$due" ]; then args+=(-f due_on="${due}T00:00:00Z"); fi
  milestone_number=$(awk -F'|' -v title="$title" '$1 == title { print $2 }' <<<"$existing")
  if [ -n "$milestone_number" ]; then
    gh api --method PATCH "repos/$REPO/milestones/$milestone_number" "${args[@]}" --silent
    echo "milestone $title updated"
  else
    gh api "repos/$REPO/milestones" -f title="$title" "${args[@]}" --silent
    echo "milestone $title created"
  fi
done

# --- Project board --------------------------------------------------------------------------------
number=$(gh project list --owner "$OWNER" --format json --jq ".projects[] | select(.title == \"$TITLE\") | .number")
if [ -z "$number" ]; then
  number=$(gh project create --owner "$OWNER" --title "$TITLE" --format json --jq .number)
  echo "project $TITLE created"
fi
gh project link "$number" --owner "$OWNER" --repo "${REPO#*/}" 2>/dev/null || true

# The short description and the README come from project-readme.md, next to this script.
gh project edit "$number" --owner "$OWNER" \
  --description "Issues and PRs for sisa-caching-rs, from triage to release." \
  --readme "$(cat "$(dirname "$0")/project-readme.md")" >/dev/null
echo "project description and README updated"

# Sync the Status options with the flow in CONTRIBUTING.md. An option that already exists keeps its
# id, so items keep their status; one not listed here (such as the default Todo) is removed, and
# its items lose their status.
field_list=$(gh project field-list "$number" --owner "$OWNER" --format json --limit 100)
status_id=$(jq -r '.fields[] | select(.name == "Status") | .id' <<<"$field_list")
have_status=$(jq -c '[.fields[] | select(.name == "Status") | (.options // [])[]]' <<<"$field_list")
jq -n --arg fieldId "$status_id" --argjson have "$have_status" '
  [
    {name: "Backlog",     color: "GRAY",   description: "New, or not planned yet."},
    {name: "Ready",       color: "BLUE",   description: "Accepted and in a milestone; anyone can start."},
    {name: "In Progress", color: "YELLOW", description: "Someone is assigned and working on it."},
    {name: "Blocked",     color: "RED",    description: "Waiting on something outside this issue; the issue says what."},
    {name: "In Review",   color: "PURPLE", description: "A pull request is open."},
    {name: "Done",        color: "GREEN",  description: "Merged or closed; shipped once its milestone is closed."}
  ]
  | map(. as $option | . + ([$have[] | select(.name == $option.name) | {id}] | first // {}))
  | {
      query: "mutation($fieldId: ID!, $options: [ProjectV2SingleSelectFieldOptionInput!]) {
        updateProjectV2Field(input: {fieldId: $fieldId, singleSelectOptions: $options}) {
          projectV2Field { ... on ProjectV2SingleSelectField { options { name } } }
        }
      }",
      variables: {fieldId: $fieldId, options: .}
    }' |
  gh api graphql --input - --jq '"status options: " + ([.data.updateProjectV2Field.projectV2Field.options[].name] | join(", "))'

# --- Fields ---------------------------------------------------------------------------------------
# Priority orders the work; Start date and Target date place it on the Roadmap view.
project_id=$(gh project view "$number" --owner "$OWNER" --format json --jq .id)
have_fields=$(jq -r '.fields[].name' <<<"$field_list")
if grep -qxF Priority <<<"$have_fields"; then
  echo "field Priority exists"
else
  gh api graphql -f projectId="$project_id" -f query='
  mutation($projectId: ID!) {
    createProjectV2Field(input: {
      projectId: $projectId
      dataType: SINGLE_SELECT
      name: "Priority"
      singleSelectOptions: [
        {name: "P0", color: RED,    description: "Urgent: breaks users now; drop other work."}
        {name: "P1", color: ORANGE, description: "High: needed for the current milestone."}
        {name: "P2", color: YELLOW, description: "Medium: planned, but can slip to the next milestone."}
        {name: "P3", color: GRAY,   description: "Low: nice to have."}
      ]
    }) { projectV2Field { ... on ProjectV2SingleSelectField { name } } }
  }' --silent
  echo "field Priority created"
fi
for name in "Start date" "Target date"; do
  if grep -qxF "$name" <<<"$have_fields"; then
    echo "field $name exists"
  else
    gh project field-create "$number" --owner "$OWNER" --name "$name" --data-type DATE >/dev/null
    echo "field $name created"
  fi
done

# --- Views ----------------------------------------------------------------------------------------
# GitHub's API can create views but not edit or delete them, so a view that exists is skipped. To
# change one, delete it on the board and run this again.
fields=$(gh api "orgs/$OWNER/projectsV2/$number/fields?per_page=100" --jq 'map({(.name): .id}) | add')
field() { jq -er --arg name "$1" '.[$name] // error("no field \($name)")' <<<"$fields"; }
title=$(field Title)
status=$(field Status)
assignees=$(field Assignees)
labels=$(field Labels)
milestone=$(field Milestone)
type=$(field Type)
linked=$(field "Linked pull requests")
reviewers=$(field Reviewers)
priority=$(field Priority)
start=$(field "Start date")
target=$(field "Target date")

views=$(gh api graphql -f owner="$OWNER" -F number="$number" -f query='
query($owner: String!, $number: Int!) {
  organization(login: $owner) { projectV2(number: $number) { views(first: 50) { nodes { name } } } }
}' --jq '.data.organization.projectV2.views.nodes[].name')

# name, then the jq program that builds the request body from the field ids.
view() {
  if grep -qxF "$1" <<<"$views"; then
    echo "view $1 exists"
    return
  fi
  jq -n --arg name "$1" --argjson title "$title" --argjson status "$status" \
    --argjson assignees "$assignees" --argjson labels "$labels" --argjson milestone "$milestone" \
    --argjson type "$type" --argjson linked "$linked" --argjson reviewers "$reviewers" --argjson priority "$priority" \
    --argjson start "$start" --argjson target "$target" "{name: \$name} + $2" \
    | gh api --method POST "orgs/$OWNER/projectsV2/$number/views" --input - --silent
  echo "view $1 created"
}

# The issues in a column per Status, the flow in CONTRIBUTING.md.
view "Board" '{layout: "board", filter: "is:issue",
  vertical_group_by: [$status], sort_by: [[$priority, "asc"]],
  visible_fields: [$title, $type, $priority, $assignees, $labels, $milestone, $linked]}'

# Triage: issues not accepted yet. Accepting one means setting its milestone and moving it to Ready.
view "Backlog" '{layout: "table", filter: "is:issue status:Backlog",
  sort_by: [[$priority, "asc"]],
  visible_fields: [$title, $type, $priority, $labels, $milestone, $assignees]}'

# Open issues still missing the Priority the maintainer sets when accepting them.
view "Needs priority" '{layout: "table", filter: "is:issue is:open no:priority",
  visible_fields: [$title, $type, $status, $milestone, $assignees]}'

# What each release ships and how far along it is.
view "Delivery" '{layout: "table", filter: "is:issue has:milestone",
  group_by: [$milestone], sort_by: [[$status, "asc"], [$priority, "asc"]],
  visible_fields: [$title, $type, $status, $priority, $labels, $assignees, $start, $target, $linked]}'

# Delivery on a timeline: each issue from its Start date to its Target date, a row per milestone.
# Milestones with a due date show as markers.
view "Roadmap" '{layout: "roadmap", filter: "is:issue has:milestone",
  group_by: [$milestone], sort_by: [[$target, "asc"]]}'

# What is stuck, and on what.
view "Blocked" '{layout: "table", filter: "status:Blocked",
  visible_fields: [$title, $priority, $assignees, $milestone, $linked]}'

# Whoever is looking: their open work.
view "My work" '{layout: "table", filter: "is:issue assignee:@me -status:Done",
  group_by: [$status], sort_by: [[$priority, "asc"]],
  visible_fields: [$title, $priority, $milestone, $target, $linked]}'

# Open pull requests and who reviews them.
view "Pull requests" '{layout: "table", filter: "is:pr is:open",
  visible_fields: [$title, $assignees, $reviewers, $labels, $milestone]}'

url=$(gh project view "$number" --owner "$OWNER" --format json --jq .url)
cat <<MSG

Done. These steps are left by hand, because GitHub has no API for them.

1. The default view "View 1" can't be edited from the API, and the first tab is the one the board
   opens on. Open $url, delete "View 1" so Board comes first, and drag the tabs into the order
   you want.

2. The API can't pick a roadmap's date fields. On the Roadmap view, open Date fields and choose
   Start date and Target date.

3. Open $url/workflows and turn on:
  - Auto-add to project: repository sisa-caching-rs, filter "is:issue,pr is:open"
  - Item added to project: issues only, set Status to Backlog
  - Item reopened: issues only, set Status to Backlog
  - Pull request linked to issue: set Status to In Review
  - Item closed: issues and pull requests, set Status to Done
  - Pull request merged: set Status to Done
  Auto-add only acts on items opened or edited after it is on; add older ones by hand.

4. After a release is published, close its milestone on GitHub.
MSG
