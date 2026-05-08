#!/usr/bin/env bash
# lint.sh — canonical lint sequence for agentstategraph.
#
# Correct order matters:
#   1. cargo fmt          — normalize whitespace / imports first
#   2. cargo clippy --fix — auto-fix lints (may reformat)
#   3. cargo fmt          — clean up any reformatting clippy introduced
#   4. cargo clippy       — final gate check (no --fix; must be green)
#
# Why this order? Running clippy --fix before fmt, or running them
# independently without --allow-dirty, leaves the repo in an intermediate
# state where fmt rewrites clippy's output and vice versa.  The double-fmt
# sequence is the only reliable way to reach a stable point where both tools
# agree.  Never declare the gate green until step 4 passes.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

echo "==> cargo fmt"
cargo fmt --all

echo "==> cargo clippy --fix"
cargo clippy --all-targets --all-features --fix --allow-dirty --allow-staged 2>&1

echo "==> cargo fmt (second pass)"
cargo fmt --all

echo "==> cargo clippy (gate check)"
cargo clippy --all-targets --all-features -- -D warnings

echo "==> lint OK"
