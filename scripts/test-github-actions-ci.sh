#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$ROOT/scripts/lib/github-actions.sh"

gh() {
  [ "$1" = api ] || { echo "unexpected gh command: $*" >&2; return 1; }
  case "$2" in
    repos/*/releases/tags/*)
      printf '%s\n' '{"assets":[
        {"name":"swift.zip","state":"uploaded","digest":"sha256:abc123"},
        {"name":"swift.zip.sha256","state":"starter","digest":"sha256:def456"},
        {"name":"swift-release.json","state":"uploaded","digest":null}
      ]}' ;;
    *)
      printf '%s\n' '{"workflow_runs":[
        {"id":41,"event":"workflow_dispatch","display_title":"verify Swift v0.9.21"},
        {"id":42,"event":"workflow_dispatch","display_title":"verify Swift v0.9.21"},
        {"id":99,"event":"push","display_title":"verify Swift v0.9.21"},
        {"id":100,"event":"workflow_dispatch","display_title":"another run"}
      ]}' ;;
  esac
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

# A re-run skips only assets GitHub holds, fully uploaded, with a known digest.
for pair in "swift.zip=abc123" "swift.zip.sha256=" "swift-release.json=" "missing.zip="; do
  name="${pair%%=*}"
  want="${pair#*=}"
  got=$(github_release_asset_sha256 example/repo v0.9.21 "$name")
  [ "$got" = "$want" ] || { echo "expected '$want' for $name, got '$got'" >&2; exit 1; }
done

# A failed artifact download is retried into a fresh directory, so only a
# complete download reaches the destination.
DOWNLOADS=$(mktemp -d)
trap 'rm -rf "$DOWNLOADS"' EXIT
gh() {
  [ "$1 $2" = "run download" ] || { echo "unexpected gh command: $*" >&2; return 1; }
  local dir="${@: -1}"
  local n=$(( $(cat "$DOWNLOADS/attempts" 2>/dev/null || echo 0) + 1 ))
  echo "$n" > "$DOWNLOADS/attempts"
  if [ "$n" -lt 3 ]; then
    echo partial > "$dir/partial.zip"
    echo "HTTP 503: 503 Service Unavailable" >&2
    return 1
  fi
  echo payload > "$dir/swift.zip"
}

mkdir "$DOWNLOADS/stage"
github_download_run_artifact example/repo 42 swift-release "$DOWNLOADS/stage" 3 0 2>/dev/null \
  || { echo "expected the third download attempt to succeed" >&2; exit 1; }
[ "$(cat "$DOWNLOADS/attempts")" = 3 ] || { echo "expected 3 download attempts" >&2; exit 1; }
got=$(ls -A "$DOWNLOADS/stage")
[ "$got" = swift.zip ] || { echo "expected only swift.zip in the stage, got '$got'" >&2; exit 1; }

rm "$DOWNLOADS/attempts"
mkdir "$DOWNLOADS/stage2"
if github_download_run_artifact example/repo 42 swift-release "$DOWNLOADS/stage2" 2 0 2>/dev/null; then
  echo "expected a download that fails every attempt to fail" >&2
  exit 1
fi
[ -z "$(ls -A "$DOWNLOADS/stage2")" ] || { echo "expected a failed download to leave the stage empty" >&2; exit 1; }

echo "GitHub Actions compatibility tests passed"
