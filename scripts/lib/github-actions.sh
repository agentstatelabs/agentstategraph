#!/usr/bin/env bash

# Query workflow runs through `gh api` rather than `gh run list --event`.
# Debian 12 ships a GitHub CLI version that does not expose the --event flag.
github_latest_workflow_run_id() {
  local repo="$1"
  local workflow="$2"
  local run_title="$3"

  gh api --method GET \
    "repos/${repo}/actions/workflows/${workflow}/runs" \
    -f event=workflow_dispatch \
    -f per_page=30 \
    | jq -r --arg title "$run_title" \
      '[.workflow_runs[] | select(.event == "workflow_dispatch" and .display_title == $title) | .id] | max // empty'
}

# A workflow dispatch is asynchronous. Only return a run created after the
# caller's snapshot so retried CI jobs cannot accidentally watch an older run.
github_wait_for_new_workflow_run() {
  local repo="$1"
  local workflow="$2"
  local run_title="$3"
  local previous_run_id="${4:-0}"
  local attempts="${5:-60}"
  local delay="${6:-5}"
  local run_id=""

  previous_run_id="${previous_run_id:-0}"
  for _ in $(seq 1 "$attempts"); do
    run_id=$(github_latest_workflow_run_id "$repo" "$workflow" "$run_title")
    if [ -n "$run_id" ] && [ "$run_id" -gt "$previous_run_id" ]; then
      printf '%s\n' "$run_id"
      return 0
    fi
    sleep "$delay"
  done
  return 1
}
