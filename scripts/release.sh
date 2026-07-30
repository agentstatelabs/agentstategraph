#!/usr/bin/env bash
#
# release.sh <X.Y.Z> — one command to cut a release.
#
# Version drift (a wheel named 0.9.8 built from a 0.9.11 tag) has bitten this
# repo before, so there is exactly ONE place a human edits the version — the
# workspace — and this script propagates it everywhere the publish pipeline
# reads a version:
#   * [workspace.package].version            (all crates inherit via version.workspace)
#   * internal deps in [workspace.dependencies] pins
#   * bindings/typescript/package.json       (npm has no way to read Cargo)
#   * CHANGELOG.md                           ([Unreleased] -> [vX.Y.Z] — DATE)
# The Python wheel needs no edit: bindings/python/pyproject.toml is
# `dynamic = ["version"]` and reads the Cargo workspace version at build time.
#
# CI then enforces this with the `version-guard` job (see .gitlab-ci.yml): a tag
# whose versions disagree fails in the `check` stage, before anything publishes.
#
# Usage:  scripts/release.sh 0.9.12
# Then:   git push --follow-tags        (this is the deploy trigger)
set -euo pipefail

[ $# -eq 1 ] || { echo "usage: $0 <X.Y.Z>"; exit 2; }
NEW="$1"
echo "$NEW" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z.-]+)?$' \
  || { echo "error: '$NEW' is not a semver version"; exit 2; }

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# Refuse to run on a dirty tree so the release commit is exactly the version bump.
if [ -n "$(git status --porcelain)" ]; then
  echo "error: working tree is dirty; commit or stash first"; exit 1
fi

OLD=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
[ -n "$OLD" ] || { echo "error: cannot read current [workspace.package].version"; exit 1; }
[ "$OLD" != "$NEW" ] || { echo "error: version is already $NEW"; exit 1; }
echo "Bumping $OLD -> $NEW"

# 1. [workspace.package].version — the single line-anchored top-level version.
perl -i -pe 'if (!$d && /^version = "\Q'"$OLD"'\E"$/) { s/"\Q'"$OLD"'\E"/"'"$NEW"'"/; $d=1 }' Cargo.toml

# 2. Internal dep pins in [workspace.dependencies].
perl -i -pe 's/(agentstategraph[\w-]* = \{ path = "[^"]*", version = )"\Q'"$OLD"'\E"/$1"'"$NEW"'"/g' Cargo.toml

# 3. TypeScript binding package.json.
perl -i -pe 's/("version": )"\Q'"$OLD"'\E"/$1"'"$NEW"'"/' bindings/typescript/package.json

# 4. CHANGELOG: open a fresh [Unreleased] and stamp the release below it.
# Byte-oriented (awk) so existing UTF-8 (em-dashes) is copied verbatim, never re-encoded.
DATE=$(date +%Y-%m-%d)
awk -v hdr="## [v$NEW] — $DATE" '
  /^## \[Unreleased\]$/ && !done { print; print ""; print hdr; done=1; next }
  { print }
' CHANGELOG.md > CHANGELOG.md.tmp && mv CHANGELOG.md.tmp CHANGELOG.md

# 5. Refresh Cargo.lock so `--locked` CI builds pass.
cargo update --workspace >/dev/null 2>&1 || true

echo
echo "Changed files:"
git --no-pager diff --stat
echo
echo "Next steps:"
echo "  git commit -am 'release: v$NEW'"
echo "  git tag v$NEW"
echo "  git push --follow-tags     # triggers the publish/deploy pipeline"
