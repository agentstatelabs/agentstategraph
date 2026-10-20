#!/usr/bin/env bash
#
# publish-crates.sh — publish this workspace's crates to crates.io.
#
# Usage: scripts/publish-crates.sh             # dry run (any clean checkout)
#        scripts/publish-crates.sh --publish   # real upload (release tag only)
#
# The publish-crates CI job runs this on a release tag; RELEASE.md covers
# running it by hand from a detached checkout of the tag. cargo takes the
# crates.io token from CARGO_REGISTRY_TOKEN (CI) or `cargo login` (by hand).
#
# Safe to re-run. Crates already on crates.io at this version are skipped, so a
# run that stopped partway (a network error, or crates.io's rate limit on new
# crate names) picks up where it left off. A crates.io version can never be
# deleted or re-uploaded, so --publish insists on committed code at the tag that
# matches the workspace version.
#
# Portable to bash 3.2 (macOS default).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

MODE="--dry-run"
case "${1:-}" in
  "") ;;
  --publish) MODE="" ;;
  *) echo "usage: $0 [--publish]" >&2; exit 2 ;;
esac

# Tracked files only: CI keeps its cargo home and caches inside the checkout,
# and cargo itself refuses to package files that are not committed.
if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
  echo "error: uncommitted changes; publish only committed code:" >&2
  git status --short --untracked-files=no >&2
  exit 1
fi

VER="$(awk '/^\[workspace\.package\]/{f=1;next} f&&/^version = /{gsub(/[",]/,"",$3);print $3;exit}' Cargo.toml)"
TAG="$(git describe --tags --exact-match HEAD 2>/dev/null || true)"
# A tag pipeline's checkout may lack the tag ref; GitLab names it for us.
if [ -z "$TAG" ] && [ -n "${CI_COMMIT_TAG:-}" ] && [ "$(git rev-parse HEAD)" = "${CI_COMMIT_SHA:-}" ]; then
  TAG="$CI_COMMIT_TAG"
fi
if [ -z "$MODE" ]; then
  [ "$TAG" = "v$VER" ] || {
    echo "error: --publish needs HEAD at tag v$VER (HEAD is ${TAG:-untagged})" >&2
    exit 1
  }
elif [ "$TAG" != "v$VER" ]; then
  echo "note: HEAD is not tag v$VER; fine for a dry run, not for --publish"
fi

# Every publishable workspace member.
CRATES="$(cargo metadata --no-deps --format-version 1 | python3 -c '
import json, sys
for p in json.load(sys.stdin)["packages"]:
    if p["publish"] != []:
        print(p["name"])
')"

# Ask crates.io which crates are already live at $VER. Sets LIVE, an --exclude
# for each of them in EXCLUDE, and TODO, the number left to publish.
check_live() {
  LIVE=""
  EXCLUDE=()
  TODO=0
  local c code
  for c in $CRATES; do
    code="$(curl -s -o /dev/null -w '%{http_code}' -A "agentstatelabs-publish-crates" \
      "https://crates.io/api/v1/crates/$c/$VER")"
    case "$code" in
      200) LIVE="$LIVE $c"; EXCLUDE+=(--exclude "$c") ;;
      404) TODO=$((TODO + 1)) ;;
      *) echo "error: crates.io answered $code for $c $VER; not publishing" >&2; exit 1 ;;
    esac
  done
}

# crates.io lists an upload at once, but cargo resolves the excluded crates from
# the sparse index, which can lag. Wait up to five minutes for each live crate
# to appear there. Every crate here has a name of four or more characters, so
# its index file is at <first two>/<next two>/<name>.
wait_for_index() {
  local c tries body
  for c in $LIVE; do
    tries=0
    while :; do
      body="$(curl -fs -A "agentstatelabs-publish-crates" \
        "https://index.crates.io/${c:0:2}/${c:2:2}/$c" || true)"
      case "$body" in *"\"vers\":\"$VER\""*) break ;; esac
      tries=$((tries + 1))
      [ "$tries" -lt 30 ] || { echo "error: $c $VER is not in the crates.io index after five minutes" >&2; exit 1; }
      sleep 10
    done
  done
}

check_live
for c in $LIVE; do echo "skip $c $VER (already on crates.io)"; done
if [ "$TODO" -eq 0 ]; then
  echo "every crate is already on crates.io at $VER; nothing to do"
  exit 0
fi

# Cargo 1.90+ packages and verifies every crate before uploading any, then
# uploads in dependency order.
#
# Cargo 1.96 can stop partway through the uploads with "no packages ready to
# publish but N packages remain in plan", an internal error (v1.4.0 and v1.4.1
# both stopped after three crates, and retrying the job published the rest).
# So after a failure, publish what is left, for as long as each run uploads at
# least one crate. A failure that uploaded nothing is real, and fails the run.
until cargo publish --workspace --locked ${EXCLUDE[@]+"${EXCLUDE[@]}"} $MODE; do
  before=$TODO
  check_live
  if [ "$TODO" -eq 0 ]; then
    echo "every crate is on crates.io at $VER despite cargo's error"
    exit 0
  fi
  if [ "$TODO" -ge "$before" ]; then
    echo "error: cargo publish failed without uploading a crate; $TODO left to publish" >&2
    exit 1
  fi
  echo "cargo publish stopped with $TODO crate(s) left; publishing them once the index has the others" >&2
  wait_for_index
done
