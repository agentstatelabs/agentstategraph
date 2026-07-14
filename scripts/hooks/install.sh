#!/usr/bin/env bash
# Install the client-side pre-push leak-scan hook into this clone.
set -euo pipefail
ROOT="$(git rev-parse --show-toplevel)"
HOOK_SRC="${ROOT}/scripts/hooks/pre-push"
HOOK_DST="$(git rev-parse --git-path hooks)/pre-push"

chmod +x "${ROOT}/scripts/leak-scan.sh" "${ROOT}/scripts/hooks/pre-push" 2>/dev/null || true
ln -sf "$HOOK_SRC" "$HOOK_DST"
echo "Installed pre-push hook -> $HOOK_DST"
echo "It runs scripts/leak-scan.sh on every push and blocks internal/private content."
echo "Bypass (discouraged): git push --no-verify"
