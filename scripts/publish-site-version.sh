#!/usr/bin/env bash
# publish-site-version — record this release in agentstatelabs.com/releases.json.
#
# The product sites (and agentstatelabs.com itself) read their "current
# version" from that one file at page load, so updating it here is the whole
# of a site version bump. Run by the site-version CI job on a release tag,
# after the release is installable.
#
# It only ever moves a version forward: a patch tag cut on an older line
# (v1.0.12 after v1.1.0 shipped) leaves the site on the newer release.
#
# Required env:
#   SITE_RELEASES_TOKEN — write_repository on $SITE_PROJECT, allowed to push
#                         to its main branch (protected, masked variable)
#   SITE_PRODUCT        — this product's key in releases.json (e.g. ctxone)
#   SITE_PROJECT        — GitLab path of the site repo
#   CI_COMMIT_TAG, CI_SERVER_URL — GitLab built-ins
set -euo pipefail

: "${SITE_RELEASES_TOKEN:?set SITE_RELEASES_TOKEN (protected variable — is this tag protected?)}"
: "${SITE_PRODUCT:?}" "${SITE_PROJECT:?}" "${CI_COMMIT_TAG:?}" "${CI_SERVER_URL:?}"

WORK="$(mktemp -d)"
# CI_SERVER_URL keeps the hostname out of the repo; leak-scan BLOCKs literals.
git clone -q --depth 1 \
  "https://oauth2:${SITE_RELEASES_TOKEN}@${CI_SERVER_URL#https://}/${SITE_PROJECT}.git" "$WORK/site"
cd "$WORK/site"
git config user.name "AgentStateLabs CI"
git config user.email "ci@agentstatelabs.com"

bump() {
  python3 - "$SITE_PRODUCT" "$CI_COMMIT_TAG" <<'EOF'
import json, re, sys
product, tag = sys.argv[1], sys.argv[2]
path = 'public/releases.json'
data = json.load(open(path))

def key(v):
    # v1.2.3 < v1.2.4-beta.1 < v1.2.4; anything unparseable sorts lowest.
    m = re.match(r'v?(\d+)\.(\d+)\.(\d+)(?:-(.+))?$', v or '')
    if not m:
        return (-1,)
    pre = m.group(4)
    return (*map(int, m.group(1, 2, 3)), 0 if pre else 1, pre or '')

current = data.get(product)
if current and key(tag) <= key(current):
    print(f'releases.json already has {product} {current}; not moving it to {tag}')
    sys.exit(0)
data[product] = tag
open(path, 'w').write(json.dumps(data, indent=2) + '\n')
print(f'{product}: {current} -> {tag}')
EOF
}

# Two products releasing at once race on main; rebase and retry.
for attempt in 1 2 3; do
  bump
  if git diff --quiet; then exit 0; fi
  git commit -qam "releases: ${SITE_PRODUCT} ${CI_COMMIT_TAG}"
  if git push -q origin HEAD:main; then
    echo "published ${SITE_PRODUCT} ${CI_COMMIT_TAG} to releases.json"
    exit 0
  fi
  echo "push rejected (attempt $attempt); refetching"
  git fetch -q --depth 1 origin main && git reset -q --hard FETCH_HEAD
done
echo "ERROR: could not push to ${SITE_PROJECT} main after 3 attempts" >&2
exit 1
