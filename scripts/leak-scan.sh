#!/usr/bin/env bash
#
# leak-scan — block internal/private content from reaching a public mirror.
#
# Reusable across repos. Two tiers:
#   BLOCK  — any hit fails the scan (exit 1). Personal paths, internal hosts,
#            internal org/email domains, secrets.
#   WARN   — reported but does NOT fail. Internal project names that may
#            legitimately appear in public marketing/docs.
#
# Modes:
#   leak-scan.sh --tree            scan tracked files in the working tree (default)
#   leak-scan.sh --range A..B       scan every commit introduced in A..B
#   leak-scan.sh --history          scan every commit reachable from all refs
#   leak-scan.sh --pre-receive      read "<old> <new> <ref>" lines on stdin (GitLab)
#
# Per-repo overrides: create .leakscan.local at the repo root to append to
# BLOCK / WARN / ALLOW_PATHS (plain bash, sourced before scanning).
#
# Portable to bash 3.2 (macOS default). Exit: 0 clean, 1 blocked, 2 usage.

set -euo pipefail

MODE="${1:---tree}"
RANGE="${2:-}"

REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
LOG_DIR="${LEAKSCAN_LOG_DIR:-${REPO_ROOT}/.git/leak-scan}"
mkdir -p "$LOG_DIR"
LOG_FILE="${LOG_DIR}/last-scan.log"

# --- BLOCK: any match fails the push --------------------------------------
# Only GENERIC, non-sensitive patterns live here (this file is public).
# Org-specific terms — internal hostnames, IP ranges, project/company names,
# private email domains — go in .leakscan.local (gitignored; distributed via
# the private enterprise repo) so the public copy never lists your secrets.
BLOCK=(
  '/Users/[A-Za-z0-9._-]+'                 # local home paths (macOS)
  '/home/[A-Za-z0-9._-]+'                  # local home paths (linux)
  '-----BEGIN [A-Z ]*PRIVATE KEY-----'     # private keys
  'AKIA[0-9A-Z]{16}'                       # AWS access key id
  'gh[pousr]_[A-Za-z0-9]{36,}'             # GitHub tokens
  'xox[baprs]-[A-Za-z0-9-]{10,}'           # Slack tokens
)

# --- WARN: reported, does not fail ----------------------------------------
WARN=()  # org-specific project names are appended by .leakscan.local

# --- ALLOW_CONTENT: matching lines are dropped BEFORE counting -------------
# Kills false positives from documentation placeholders (e.g. /Users/user in a
# doc example is not a real home-path leak). Extend via .leakscan.local.
ALLOW_CONTENT=(
  '/(Users|home)/(x|you|me|user|username|name|alice|bob|carol|example|USER|USERNAME)([/."'\''<> ]|$)'
)

# --- ALLOW: pathspecs never scanned (this tool + noise) -------------------
ALLOW_PATHS=(
  ':!scripts/leak-scan.sh'
  ':!scripts/hooks'
  ':!.leakscan.local'
  ':!*.lock'
)

# Per-repo overrides (append to arrays above).
[ -f "${REPO_ROOT}/.leakscan.local" ] && . "${REPO_ROOT}/.leakscan.local"

hits_block=0
hits_warn=0
: >"$LOG_FILE"

log() { printf '%s\n' "$*" | tee -a "$LOG_FILE"; }

# scan_ref <BLOCK|WARN> <committish|"">   ("" = working tree)
scan_ref() {
  tier="$1"; ref="$2"
  # The `${arr[@]+"${arr[@]}"}` form expands to nothing when the array is
  # empty/unset instead of tripping `set -u` — required on bash 3.2 (macOS
  # default), where a bare "${arr[@]}" on an empty array is an "unbound
  # variable" error. Bites any repo whose .leakscan.local leaves a tier empty.
  if [ "$tier" = BLOCK ]; then set -- ${BLOCK[@]+"${BLOCK[@]}"}; else set -- ${WARN[@]+"${WARN[@]}"}; fi
  label="$ref"; [ -z "$ref" ] && label="(working tree)"
  for p in "$@"; do
    if [ -n "$ref" ]; then
      out=$(git grep -nIE -e "$p" "$ref" -- "${ALLOW_PATHS[@]}" 2>/dev/null || true)
    else
      out=$(git grep -nIE -e "$p" -- "${ALLOW_PATHS[@]}" 2>/dev/null || true)
    fi
    [ -z "$out" ] && continue
    # Drop lines matching an ALLOW_CONTENT placeholder before counting.
    if [ "${#ALLOW_CONTENT[@]}" -gt 0 ]; then
      allow_re=$(printf '%s|' "${ALLOW_CONTENT[@]}"); allow_re="${allow_re%|}"
      out=$(printf '%s\n' "$out" | grep -vE "$allow_re" || true)
      [ -z "$out" ] && continue
    fi
    while IFS= read -r line; do
      # For a committish, git grep already prefixes "<commit>:"; only the
      # working-tree scan needs a label.
      if [ -z "$ref" ]; then log "  [$tier] (working tree): ${line}"; else log "  [$tier] ${line}"; fi
      if [ "$tier" = BLOCK ]; then hits_block=$((hits_block+1)); else hits_warn=$((hits_warn+1)); fi
    done <<EOF
$out
EOF
  done
}

# Build the list of refs to scan ("" means working tree).
refs=()
case "$MODE" in
  --tree)    refs=("") ;;
  --range)
    [ -n "$RANGE" ] || { echo "usage: leak-scan.sh --range A..B" >&2; exit 2; }
    # Unquoted so callers can pass full rev-list expressions, e.g.
    # "SHA --not --remotes" for a brand-new branch.
    while IFS= read -r c; do refs+=("$c"); done < <(git rev-list $RANGE 2>/dev/null) ;;
  --history)
    while IFS= read -r c; do refs+=("$c"); done < <(git rev-list --all) ;;
  --pre-receive)
    while read -r _old new _ref; do
      [ "$new" = "0000000000000000000000000000000000000000" ] && continue
      while IFS= read -r c; do refs+=("$c"); done < <(git rev-list "$new" --not --all 2>/dev/null || true)
    done ;;
  *) echo "unknown mode: $MODE" >&2; exit 2 ;;
esac

log "leak-scan: mode=$MODE refs=${#refs[@]}"
for r in ${refs[@]+"${refs[@]}"}; do
  scan_ref BLOCK "$r"
  scan_ref WARN "$r"
done

log ""
log "leak-scan: ${hits_block} BLOCK hit(s), ${hits_warn} WARN hit(s)  (log: ${LOG_FILE})"

# Notification hook — set LEAKSCAN_NOTIFY to an executable to wire email later.
if [ -n "${LEAKSCAN_NOTIFY:-}" ] && [ -x "${LEAKSCAN_NOTIFY}" ]; then
  "$LEAKSCAN_NOTIFY" "$LOG_FILE" "$hits_block" "$hits_warn" || true
fi

if [ "$hits_block" -gt 0 ]; then
  log "BLOCKED: push refused — remove the flagged content (or move it to the private repo)."
  exit 1
fi
exit 0
