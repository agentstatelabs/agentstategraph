#!/usr/bin/env bash
#
# GitLab release-preparation orchestrator. GitLab remains the release authority;
# GitHub Actions is used only as a remote macOS build worker.
set -euo pipefail

required=(
  CI_API_V4_URL CI_COMMIT_SHA CI_JOB_TOKEN CI_PROJECT_ID CI_PROJECT_PATH
  CI_SERVER_URL GITHUB_REPO GITHUB_TOKEN GITLAB_RELEASE_TOKEN
)
for name in "${required[@]}"; do
  [ -n "${!name:-}" ] || { echo "error: required variable $name is unset" >&2; exit 2; }
done

export GH_TOKEN="$GITHUB_TOKEN"
ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
TAG="v$VERSION"
ASSET="agentstategraph-swift-${TAG}.xcframework.zip"
ARTIFACT="swift-release-${TAG}-${CI_COMMIT_SHA}"
RUN_TITLE="prepare Swift ${TAG} @ ${CI_COMMIT_SHA}"
PACKAGE_BASE="${CI_API_V4_URL}/projects/${CI_PROJECT_ID}/packages/generic/swift-xcframework/${VERSION}"

echo "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z.-]+)?$' || {
  echo "error: invalid workspace version '$VERSION'" >&2
  exit 2
}
echo "$CI_COMMIT_SHA" | grep -Eq '^[0-9a-f]{40}$' || {
  echo "error: invalid CI_COMMIT_SHA '$CI_COMMIT_SHA'" >&2
  exit 2
}

if git rev-parse "$TAG" >/dev/null 2>&1; then
  echo "error: tag $TAG already exists" >&2
  exit 1
fi

github_sha=$(gh api "repos/${GITHUB_REPO}/commits/main" --jq .sha)
if [ "$github_sha" != "$CI_COMMIT_SHA" ]; then
  echo "error: GitHub main is ${github_sha}, expected mirrored preparation commit ${CI_COMMIT_SHA}" >&2
  exit 1
fi

echo ">> dispatching GitHub macOS build for $TAG at $CI_COMMIT_SHA"
gh workflow run prepare-swift.yml \
  --repo "$GITHUB_REPO" \
  --ref main \
  -f "version=$VERSION" \
  -f "source_sha=$CI_COMMIT_SHA"

run_id=""
for _ in $(seq 1 60); do
  run_id=$(gh run list \
    --repo "$GITHUB_REPO" \
    --workflow prepare-swift.yml \
    --event workflow_dispatch \
    --limit 30 \
    --json databaseId,displayTitle \
    --jq ".[] | select(.displayTitle == \"$RUN_TITLE\") | .databaseId" \
    | head -1)
  [ -n "$run_id" ] && break
  sleep 5
done
[ -n "$run_id" ] || { echo "error: dispatched GitHub workflow run was not found" >&2; exit 1; }

echo ">> waiting for GitHub Actions run $run_id"
gh run watch "$run_id" --repo "$GITHUB_REPO" --exit-status

STAGE=$(mktemp -d -t agentstategraph-swift-release.XXXXXX)
cleanup() {
  git remote remove release-origin >/dev/null 2>&1 || true
  rm -rf "$STAGE"
}
trap cleanup EXIT

gh run download "$run_id" --repo "$GITHUB_REPO" --name "$ARTIFACT" --dir "$STAGE"

for file in "$ASSET" "${ASSET}.sha256" swift-release.json; do
  [ -f "$STAGE/$file" ] || { echo "error: downloaded artifact lacks $file" >&2; exit 1; }
done

metadata_version=$(jq -r .version "$STAGE/swift-release.json")
metadata_sha=$(jq -r .source_sha "$STAGE/swift-release.json")
metadata_asset=$(jq -r .asset "$STAGE/swift-release.json")
metadata_checksum=$(jq -r .checksum "$STAGE/swift-release.json")
[ "$metadata_version" = "$VERSION" ] || { echo "error: artifact version mismatch" >&2; exit 1; }
[ "$metadata_sha" = "$CI_COMMIT_SHA" ] || { echo "error: artifact source SHA mismatch" >&2; exit 1; }
[ "$metadata_asset" = "$ASSET" ] || { echo "error: artifact filename mismatch" >&2; exit 1; }

actual_checksum=$(sha256sum "$STAGE/$ASSET" | awk '{print $1}')
[ "$actual_checksum" = "$metadata_checksum" ] || { echo "error: artifact checksum mismatch" >&2; exit 1; }
grep -Eq "^${metadata_checksum}  ${ASSET}$" "$STAGE/${ASSET}.sha256" || {
  echo "error: checksum sidecar does not describe $ASSET" >&2
  exit 1
}

stage_package_file() {
  local file="$1"
  local url="${PACKAGE_BASE}/${file}"
  local existing="$STAGE/existing-${file}"
  local status
  status=$(curl --silent --show-error --output "$existing" --write-out '%{http_code}' \
    --header "JOB-TOKEN: ${CI_JOB_TOKEN}" "$url")
  case "$status" in
    200)
      cmp "$STAGE/$file" "$existing" || {
        echo "error: staged package $file already exists with different bytes" >&2
        exit 1
      }
      echo ">> GitLab package already contains identical $file"
      ;;
    404)
      curl --fail --silent --show-error \
        --header "JOB-TOKEN: ${CI_JOB_TOKEN}" \
        --upload-file "$STAGE/$file" "$url"
      echo ">> staged $file in GitLab Generic Package Registry"
      ;;
    *)
      echo "error: GitLab package lookup for $file returned HTTP $status" >&2
      exit 1
      ;;
  esac
}

stage_package_file "$ASSET"
stage_package_file "${ASSET}.sha256"
stage_package_file swift-release.json

GITHUB_REPO="$GITHUB_REPO" scripts/render-swift-package.sh \
  "$VERSION" "$metadata_checksum" "$STAGE/Package.swift"

case "$CI_SERVER_URL" in
  https://*) push_url="https://oauth2:${GITLAB_RELEASE_TOKEN}@${CI_SERVER_URL#https://}/${CI_PROJECT_PATH}.git" ;;
  http://*) push_url="http://oauth2:${GITLAB_RELEASE_TOKEN}@${CI_SERVER_URL#http://}/${CI_PROJECT_PATH}.git" ;;
  *) echo "error: unsupported CI_SERVER_URL '$CI_SERVER_URL'" >&2; exit 2 ;;
esac

git remote add release-origin "$push_url"
git fetch --quiet release-origin main --tags
remote_main=$(git rev-parse refs/remotes/release-origin/main)
if [ "$remote_main" != "$CI_COMMIT_SHA" ]; then
  echo "error: GitLab main advanced to $remote_main while preparing $CI_COMMIT_SHA" >&2
  exit 1
fi
if git ls-remote --exit-code --tags release-origin "refs/tags/$TAG" >/dev/null 2>&1; then
  echo "error: remote tag $TAG already exists" >&2
  exit 1
fi

cp "$STAGE/Package.swift" Package.swift
cp "$STAGE/swift-release.json" bindings/swift/release.json
git add Package.swift bindings/swift/release.json
git diff --cached --quiet && { echo "error: release manifest did not change" >&2; exit 1; }

git config user.name "agentstategraph-release-bot"
git config user.email "release-bot@agentstatelabs.com"
git commit -m "release: $TAG"
git tag -a "$TAG" -m "release: $TAG"

echo ">> pushing manifest commit and $TAG atomically to GitLab"
git push --atomic release-origin HEAD:refs/heads/main "refs/tags/$TAG"
echo ">> Swift release prepared; the $TAG pipeline will publish the staged artifact"
