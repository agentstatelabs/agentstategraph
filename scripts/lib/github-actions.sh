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

# Print the sha256 GitHub recorded for a fully uploaded release asset. Prints
# nothing when the release has no such asset, the upload never finished, or the
# asset predates GitHub's asset digests.
github_release_asset_sha256() {
  local repo="$1"
  local tag="$2"
  local name="$3"

  gh api "repos/${repo}/releases/tags/${tag}" \
    | jq -r --arg name "$name" \
      '.assets[] | select(.name == $name and .state == "uploaded") | .digest // empty | ltrimstr("sha256:")'
}

# Download a workflow run's artifact into dir, retrying with a doubling delay.
# GitHub has answered a download with HTTP 503 after a build that passed, which
# shipped v1.4.0 without its Swift binary. Each attempt downloads into a fresh
# directory, so a partial download is never used.
github_download_run_artifact() {
  local repo="$1"
  local run_id="$2"
  local name="$3"
  local dir="$4"
  local attempts="${5:-5}"
  local delay="${6:-15}"
  local try tmp

  for try in $(seq 1 "$attempts"); do
    tmp=$(mktemp -d "${dir}/download.XXXXXX")
    if gh run download "$run_id" --repo "$repo" --name "$name" --dir "$tmp"; then
      mv "$tmp"/* "$dir"/ || return 1
      rmdir "$tmp"
      return 0
    fi
    rm -rf "$tmp"
    if [ "$try" -lt "$attempts" ]; then
      echo "gh run download failed (attempt $try of $attempts); retrying in ${delay}s" >&2
      sleep "$delay"
      delay=$((delay * 2))
    fi
  done
  return 1
}
