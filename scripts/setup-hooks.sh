#!/usr/bin/env bash
# Point git at the repo's tracked hooks (.githooks/). Run once after cloning:
#   scripts/setup-hooks.sh
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
git config core.hooksPath .githooks
chmod +x .githooks/* 2>/dev/null || true
echo "✓ core.hooksPath = .githooks (pre-push gate active for this clone)"
