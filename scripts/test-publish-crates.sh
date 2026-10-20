#!/usr/bin/env bash
#
# Contract test for scripts/publish-crates.sh, with cargo, curl and git stubbed
# on PATH. A run that cargo stops partway publishes the crates left, a failure
# that uploaded nothing is not retried, and crates already live are skipped.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VER="$(awk '/^\[workspace\.package\]/{f=1;next} f&&/^version = /{gsub(/[",]/,"",$3);print $3;exit}' "$ROOT/Cargo.toml")"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
mkdir "$WORK/bin"

cat > "$WORK/bin/git" <<'STUB'
#!/usr/bin/env bash
# A clean tree, with HEAD at the release tag.
case "$1" in
  status) ;;
  describe) echo "v$FAKE_VER" ;;
  rev-parse) echo 0000000000000000000000000000000000000000 ;;
  *) echo "unexpected git $*" >&2; exit 1 ;;
esac
STUB

cat > "$WORK/bin/curl" <<'STUB'
#!/usr/bin/env bash
# crates.io knows the crates in $FAKE_STATE/live, and so does the index.
url="${@: -1}"
case "$url" in
  https://crates.io/api/v1/crates/*)
    c="${url#https://crates.io/api/v1/crates/}"
    c="${c%%/*}"
    if [ -e "$FAKE_STATE/live/$c" ]; then printf 200; else printf 404; fi ;;
  https://index.crates.io/*)
    c="${url##*/}"
    [ "$url" = "https://index.crates.io/${c:0:2}/${c:2:2}/$c" ] || { echo "bad index path $url" >&2; exit 1; }
    [ -e "$FAKE_STATE/live/$c" ] || exit 22
    printf '{"name":"%s","vers":"%s","deps":[]}\n' "$c" "$FAKE_VER" ;;
  *) echo "unexpected curl $*" >&2; exit 1 ;;
esac
STUB

cat > "$WORK/bin/cargo" <<'STUB'
#!/usr/bin/env bash
# Four publishable crates and one with publish = false. Each `cargo publish`
# takes the next line of $FAKE_STATE/plan: the crates it uploads, then its
# exit status.
case "$1" in
  metadata)
    printf '%s' '{"packages":[{"name":"asg-core","publish":null},{"name":"asg-tasks","publish":null},{"name":"asg-mcp","publish":null},{"name":"asg-ffi","publish":null},{"name":"asg-python","publish":[]}]}' ;;
  publish)
    echo "$*" >> "$FAKE_STATE/calls"
    n=$(( $(wc -l < "$FAKE_STATE/calls") ))
    set -- $(sed -n "${n}p" "$FAKE_STATE/plan")
    while [ $# -gt 1 ]; do touch "$FAKE_STATE/live/$1"; shift; done
    exit "$1" ;;
  *) echo "unexpected cargo $*" >&2; exit 1 ;;
esac
STUB
chmod +x "$WORK/bin/"*

# run_case <name> "<crates already live>" <plan line>...
# Leaves the script's exit status in STATUS and its cargo publish calls in
# $FAKE_STATE/calls.
run_case() {
  FAKE_STATE="$WORK/$1"
  mkdir -p "$FAKE_STATE/live"
  : > "$FAKE_STATE/calls"
  for c in $2; do touch "$FAKE_STATE/live/$c"; done
  shift 2
  printf '%s\n' "$@" > "$FAKE_STATE/plan"
  STATUS=0
  PATH="$WORK/bin:$PATH" FAKE_STATE="$FAKE_STATE" FAKE_VER="$VER" \
    bash "$ROOT/scripts/publish-crates.sh" --publish > "$FAKE_STATE/out" 2>&1 || STATUS=$?
}
fail() {
  echo "publish-crates: $*" >&2
  sed 's/^/  | /' "$FAKE_STATE/out" >&2
  exit 1
}
calls() { echo $(( $(wc -l < "$FAKE_STATE/calls") )); }
call() { sed -n "${1}p" "$FAKE_STATE/calls"; }

# cargo stops twice partway, as cargo 1.96 did on v1.4.0 and v1.4.1. Each run
# after the first excludes what is live by then.
run_case partial "" "asg-core 101" "asg-tasks 101" "asg-mcp asg-ffi 0"
[ "$STATUS" = 0 ] || fail "expected a run cargo stopped partway to finish, got exit $STATUS"
[ "$(calls)" = 3 ] || fail "expected 3 cargo publish runs, got $(calls)"
[ "$(call 1)" = "publish --workspace --locked" ] || fail "unexpected first run: $(call 1)"
[ "$(call 2)" = "publish --workspace --locked --exclude asg-core" ] || fail "unexpected second run: $(call 2)"
[ "$(call 3)" = "publish --workspace --locked --exclude asg-core --exclude asg-tasks" ] \
  || fail "unexpected third run: $(call 3)"

# A failure that uploads nothing (a build error, say) fails without a retry.
run_case nothing "" "101"
[ "$STATUS" = 1 ] || fail "expected a failure that uploaded nothing to fail, got exit $STATUS"
[ "$(calls)" = 1 ] || fail "expected no retry after a failure that uploaded nothing, got $(calls) runs"

# cargo errors after uploading everything: nothing is left to do.
run_case all-then-error "" "asg-core asg-tasks asg-mcp asg-ffi 101"
[ "$STATUS" = 0 ] || fail "expected success once every crate is live, got exit $STATUS"
[ "$(calls)" = 1 ] || fail "expected 1 cargo publish run, got $(calls)"

# A re-run skips crates already live, and publishes nothing when all are.
run_case rerun "asg-core asg-tasks" "asg-mcp asg-ffi 0"
[ "$STATUS" = 0 ] || fail "expected a re-run to finish, got exit $STATUS"
[ "$(call 1)" = "publish --workspace --locked --exclude asg-core --exclude asg-tasks" ] \
  || fail "unexpected re-run: $(call 1)"
run_case done "asg-core asg-tasks asg-mcp asg-ffi"
[ "$STATUS" = 0 ] && [ "$(calls)" = 0 ] || fail "expected nothing to publish when every crate is live"

echo "publish-crates tests passed"
