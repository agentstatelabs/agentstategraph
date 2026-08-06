#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$ROOT/scripts/lib/github-actions.sh"

gh() {
  [ "$1" = api ] || { echo "unexpected gh command: $*" >&2; return 1; }
  printf '%s\n' '{"workflow_runs":[
    {"id":41,"event":"workflow_dispatch","display_title":"verify Swift v0.9.21"},
    {"id":42,"event":"workflow_dispatch","display_title":"verify Swift v0.9.21"},
    {"id":99,"event":"push","display_title":"verify Swift v0.9.21"},
    {"id":100,"event":"workflow_dispatch","display_title":"another run"}
  ]}'
}

latest=$(github_latest_workflow_run_id example/repo verify.yml "verify Swift v0.9.21")
[ "$latest" = 42 ] || { echo "expected latest matching run 42, got '$latest'" >&2; exit 1; }

new_run=$(github_wait_for_new_workflow_run \
  example/repo verify.yml "verify Swift v0.9.21" 41 1 0)
[ "$new_run" = 42 ] || { echo "expected new run 42, got '$new_run'" >&2; exit 1; }

if github_wait_for_new_workflow_run \
  example/repo verify.yml "verify Swift v0.9.21" 42 1 0 >/dev/null; then
  echo "expected an unchanged run list to time out" >&2
  exit 1
fi

echo "GitHub Actions compatibility tests passed"
