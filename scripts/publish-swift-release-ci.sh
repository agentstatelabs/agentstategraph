#!/usr/bin/env bash
# Publish the exact Swift artifact prepared before the release tag, then verify
# that a clean external SwiftPM consumer can resolve, build, and run it.
set -euo pipefail

required=(CI_API_V4_URL CI_COMMIT_SHA CI_COMMIT_TAG CI_JOB_TOKEN CI_PROJECT_ID GITHUB_REPO GITHUB_TOKEN)
for name in "${required[@]}"; do
  [ -n "${!name:-}" ] || { echo "error: required variable $name is unset" >&2; exit 2; }
done

export GH_TOKEN="$GITHUB_TOKEN"
ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"
source scripts/lib/github-actions.sh

VERSION="${CI_COMMIT_TAG#v}"
TAG="v$VERSION"
ASSET="agentstategraph-swift-${TAG}.xcframework.zip"
PACKAGE_BASE="${CI_API_V4_URL}/projects/${CI_PROJECT_ID}/packages/generic/swift-xcframework/${VERSION}"

[ "$CI_COMMIT_TAG" = "$TAG" ] || { echo "error: expected a v-prefixed release tag" >&2; exit 2; }

STAGE=$(mktemp -d -t agentstategraph-swift-publish.XXXXXX)
trap 'rm -rf "$STAGE"' EXIT

for file in "$ASSET" "${ASSET}.sha256" swift-release.json; do
  curl --fail --silent --show-error \
    --header "JOB-TOKEN: ${CI_JOB_TOKEN}" \
    --output "$STAGE/$file" \
    "${PACKAGE_BASE}/${file}"
done

metadata_version=$(jq -r .version "$STAGE/swift-release.json")
source_sha=$(jq -r .source_sha "$STAGE/swift-release.json")
metadata_asset=$(jq -r .asset "$STAGE/swift-release.json")
metadata_checksum=$(jq -r .checksum "$STAGE/swift-release.json")
[ "$metadata_version" = "$VERSION" ] || { echo "error: staged version mismatch" >&2; exit 1; }
[ "$metadata_asset" = "$ASSET" ] || { echo "error: staged asset name mismatch" >&2; exit 1; }
[ "$(sha256sum "$STAGE/$ASSET" | awk '{print $1}')" = "$metadata_checksum" ] || {
  echo "error: staged artifact checksum mismatch" >&2
  exit 1
}

manifest_version=$(sed -n 's/^let nativeVersion = "\(.*\)"/\1/p' Package.swift)
manifest_checksum=$(sed -n 's/^let nativeChecksum = "\(.*\)"/\1/p' Package.swift)
[ "$manifest_version" = "$VERSION" ] || { echo "error: Package.swift version mismatch" >&2; exit 1; }
[ "$manifest_checksum" = "$metadata_checksum" ] || { echo "error: Package.swift checksum mismatch" >&2; exit 1; }
cmp "$STAGE/swift-release.json" bindings/swift/release.json || {
  echo "error: committed release metadata differs from staged metadata" >&2
  exit 1
}

git cat-file -e "${source_sha}^{commit}" 2>/dev/null || {
  echo "error: prepared source commit $source_sha is absent from tag history" >&2
  exit 1
}
git merge-base --is-ancestor "$source_sha" "$CI_COMMIT_SHA" || {
  echo "error: prepared source commit is not an ancestor of the release tag" >&2
  exit 1
}
unexpected=$(git diff --name-only "$source_sha" "$CI_COMMIT_SHA" \
  | grep -Ev '^(Package.swift|bindings/swift/release.json)$' || true)
[ -z "$unexpected" ] || {
  echo "error: source changed after the Swift binary was built:" >&2
  echo "$unexpected" >&2
  exit 1
}

echo ">> waiting for mirrored GitHub tag $TAG"
for _ in $(seq 1 60); do
  gh api "repos/${GITHUB_REPO}/git/ref/tags/${TAG}" >/dev/null 2>&1 && break
  sleep 5
done
gh api "repos/${GITHUB_REPO}/git/ref/tags/${TAG}" >/dev/null

if ! gh release view "$TAG" --repo "$GITHUB_REPO" >/dev/null 2>&1; then
  gh release create "$TAG" --repo "$GITHUB_REPO" --verify-tag --title "$TAG" --generate-notes
fi
gh release upload "$TAG" \
  --repo "$GITHUB_REPO" \
  --clobber \
  "$STAGE/$ASSET" \
  "$STAGE/${ASSET}.sha256" \
  "$STAGE/swift-release.json"

echo ">> dispatching clean remote SwiftPM verification"
run_title="verify Swift $TAG"
previous_run_id=$(github_latest_workflow_run_id \
  "$GITHUB_REPO" verify-swift-release.yml "$run_title")
gh workflow run verify-swift-release.yml \
  --repo "$GITHUB_REPO" \
  --ref main \
  -f "version=$VERSION"

run_id=$(github_wait_for_new_workflow_run \
  "$GITHUB_REPO" verify-swift-release.yml "$run_title" "$previous_run_id") || {
  echo "error: verification workflow run was not found" >&2
  exit 1
}
gh run watch "$run_id" --repo "$GITHUB_REPO" --exit-status

echo ">> published and verified Swift package $TAG"
