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

# --- leak-scan gate: scan working tree + full history of what we will push ---
echo ">> leak-scan: working tree"
scripts/leak-scan.sh --tree
echo ">> leak-scan: full history of HEAD"
scripts/leak-scan.sh --range HEAD
echo ">> leak-scan clean"

# --- configure the GitHub remote (token embedded only in-process) ------------
GH_URL="https://x-access-token:${GITHUB_TOKEN}@github.com/${GITHUB_REPO}.git"
git remote remove github 2>/dev/null || true
git remote add github "$GH_URL"

# --- push ONLY main (fast-forward only) --------------------------------------
# --force-with-lease is deliberately NOT used: a non-fast-forward means GitHub
# diverged unexpectedly — fail loudly rather than overwrite.
echo ">> pushing main -> github"
git push github "HEAD:refs/heads/main"

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
