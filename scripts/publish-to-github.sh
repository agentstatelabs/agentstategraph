#!/usr/bin/env bash
#
# publish-to-github — gated push of a public repo's main branch + release tags
# to GitHub. Replaces GitLab push mirroring so that:
#   * ONLY main and release tags reach GitHub (never internal branches such as
#     claude/* or worktree-agent-*, which push mirroring would expose), and
#   * NOTHING pushes until scripts/leak-scan.sh passes (fail closed).
#
# Intended to run in GitLab CI (see .gitlab-ci.yml `publish-github`). Requires:
#   GITHUB_REPO   — "org/name" of the target GitHub repo
#   GITHUB_TOKEN  — PAT with `repo` scope (masked + protected CI variable)
#   TAG_PREFIX    — optional; only tags matching this are published (default: v)
#
# Exit: 0 pushed (or nothing to do), 1 leak-scan blocked, 2 misconfig.
set -euo pipefail

: "${GITHUB_REPO:?set GITHUB_REPO (org/name)}"
: "${GITHUB_TOKEN:?set GITHUB_TOKEN}"
TAG_PREFIX="${TAG_PREFIX:-v}"

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

# --- configure the GitHub remote (token embedded only in-process) ------------
GH_URL="https://x-access-token:${GITHUB_TOKEN}@github.com/${GITHUB_REPO}.git"
git remote remove github 2>/dev/null || true
git remote add github "$GH_URL"

# --- leak-scan gate (fail closed) --------------------------------------------
# Scan the working tree, plus the COMMITS ABOUT TO BECOME PUBLIC — not all of
# HEAD's history every time. GitHub's current main was already scanned when it
# was published, so re-scanning it on every push is redundant and, on a large
# repo, catastrophically slow (a 476-commit full-history scan ran for minutes).
#
# Incremental only when GitHub's main is a true ancestor of HEAD (the normal
# fast-forward case). First publish (no remote main) or any divergence falls
# back to a full HEAD scan — fail safe, never fail open.
echo ">> leak-scan: working tree"
scripts/leak-scan.sh --tree

GH_MAIN="$(git ls-remote github refs/heads/main 2>/dev/null | cut -f1)"
if [ -n "$GH_MAIN" ] && git cat-file -e "$GH_MAIN" 2>/dev/null \
     && git merge-base --is-ancestor "$GH_MAIN" HEAD 2>/dev/null; then
  echo ">> leak-scan: new commits ${GH_MAIN:0:12}..HEAD"
  scripts/leak-scan.sh --range "${GH_MAIN}..HEAD"
else
  echo ">> leak-scan: full history of HEAD (first publish or divergence)"
  scripts/leak-scan.sh --range HEAD
fi
echo ">> leak-scan clean"

# --- push main, but only when we actually advance it -------------------------
# Pipelines can run out of order on a busy runner: an older commit's pipeline
# may execute AFTER a newer commit already advanced GitHub's main. Pushing the
# older HEAD is a non-fast-forward — but GitHub already has newer content, so
# that is a no-op, not a failure. Decide from the HEAD<->GH_MAIN relationship;
# --force-with-lease is still deliberately NOT used, so only a genuine
# divergence (neither ref an ancestor of the other) is fatal.
HEAD_SHA="$(git rev-parse HEAD)"
if [ -z "$GH_MAIN" ]; then
  echo ">> pushing main -> github (first publish)"
  git push github "HEAD:refs/heads/main"
elif [ "$GH_MAIN" = "$HEAD_SHA" ]; then
  echo ">> github main already at HEAD (${HEAD_SHA:0:12}) — nothing to push"
elif ! git cat-file -e "$GH_MAIN" 2>/dev/null; then
  echo ">> github main ${GH_MAIN:0:12} not present locally; attempting push"
  git push github "HEAD:refs/heads/main"
elif git merge-base --is-ancestor "$GH_MAIN" HEAD 2>/dev/null; then
  echo ">> pushing main -> github (fast-forward ${GH_MAIN:0:12}..${HEAD_SHA:0:12})"
  git push github "HEAD:refs/heads/main"
elif git merge-base --is-ancestor HEAD "$GH_MAIN" 2>/dev/null; then
  echo ">> github main (${GH_MAIN:0:12}) is ahead of HEAD (${HEAD_SHA:0:12}) —"
  echo ">> stale/out-of-order pipeline, main already current; skipping main push"
else
  echo "ERROR: github main (${GH_MAIN:0:12}) has diverged from HEAD (${HEAD_SHA:0:12});" >&2
  echo "       refusing to overwrite. Reconcile the GitHub repo manually." >&2
  exit 1
fi

# --- push release tags matching TAG_PREFIX -----------------------------------
if [ -n "${CI_COMMIT_TAG:-}" ]; then
  case "$CI_COMMIT_TAG" in
    "${TAG_PREFIX}"*)
      echo ">> pushing tag ${CI_COMMIT_TAG} -> github"
      git push github "refs/tags/${CI_COMMIT_TAG}" ;;
    *) echo ">> tag ${CI_COMMIT_TAG} does not match ${TAG_PREFIX}* — not published" ;;
  esac
fi

git remote remove github 2>/dev/null || true
echo ">> publish complete"
