#!/usr/bin/env bash
#
# release.sh <X.Y.Z> — prepare the version bump for the two-phase release.
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
# CI builds the Apple XCFramework on GitHub before the final tag exists, stages
# the exact bytes in GitLab, generates the checksum-pinned root Package.swift,
# then creates the final release commit and tag.
#
# Deliberately NOT propagated: bindings/capabilities.json reviewed_core_version.
# The binding-contract gate requires it to equal the new version, and it exists
# to force a human audit of every binding — so a person bumps it, after the
# audit, in the same release-prep commit as this version bump. The release jobs
# run only when main's HEAD title matches ^release-prep: v, so a review pushed
# as a follow-up commit passes CI and tags nothing (v1.2.0, v1.2.2).
# CONTRIBUTING.md documents the audit and how to recover a release that tagged
# nothing.
#
# Usage:  scripts/release.sh 0.9.17
# Then:   audit the bindings, set reviewed_core_version to 0.9.17 by hand,
#         git commit -am "release-prep: v0.9.17" && git push origin main
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
echo "  1. Audit every binding against v$NEW (docs/BINDING_RELEASE_POLICY.md), then"
echo "     set \"reviewed_core_version\": \"$NEW\" in bindings/capabilities.json by hand."
echo "     This script never bumps it: the gate exists to force that review."
echo "  2. python3 scripts/check-binding-capabilities.py"
echo "  3. git commit -am 'release-prep: v$NEW'"
echo "     The review goes in THIS commit, with the audit's conclusions in its body."
echo "     The release jobs run only when main's HEAD title matches ^release-prep: v,"
echo "     so a review pushed as a follow-up commit passes CI but tags nothing."
echo "  4. git push origin main"
echo
echo "GitLab will mirror the preparation commit, dispatch the GitHub macOS build,"
echo "stage the exact artifact, generate Package.swift, and create/push v$NEW."
echo
echo "If no v$NEW tag appears (binding-contract failed, or the review landed"
echo "separately), push a new 'release-prep: v$NEW' commit — empty if the review"
echo "is already on main. See \"If a release tagged nothing\" in CONTRIBUTING.md."
