#!/bin/sh
# Source in a fresh CI job before starting the sccache server. Never enable xtrace.
if [ "${CI_COMMIT_REF_PROTECTED:-false}" = true ] && [ -n "${RUST_CACHE_BUCKET:-}" ]; then
  export SCCACHE_BUCKET="$RUST_CACHE_BUCKET" SCCACHE_ENDPOINT="$RUST_CACHE_ENDPOINT"
  export SCCACHE_REGION=us-east-1 SCCACHE_S3_USE_SSL=true
  export AWS_ACCESS_KEY_ID="$RUST_CACHE_ACCESS_KEY_ID" AWS_SECRET_ACCESS_KEY="$RUST_CACHE_SECRET_ACCESS_KEY"
elif [ -n "${RUST_UNTRUSTED_CACHE_BUCKET:-}" ]; then
  export SCCACHE_BUCKET="$RUST_UNTRUSTED_CACHE_BUCKET" SCCACHE_ENDPOINT="$RUST_UNTRUSTED_CACHE_ENDPOINT"
  export SCCACHE_REGION=us-east-1 SCCACHE_S3_USE_SSL=true
  export AWS_ACCESS_KEY_ID="$RUST_UNTRUSTED_CACHE_ACCESS_KEY_ID" AWS_SECRET_ACCESS_KEY="$RUST_UNTRUSTED_CACHE_SECRET_ACCESS_KEY"
fi
export CARGO_INCREMENTAL=0 RUSTC_WRAPPER=sccache SCCACHE_IGNORE_SERVER_IO_ERROR=1
export SCCACHE_DIR="${SCCACHE_DIR:-${CI_PROJECT_DIR:-/work}/.sccache}"
export SCCACHE_CACHE_SIZE="${SCCACHE_CACHE_SIZE:-1G}"
if [ -n "${SCCACHE_BUCKET:-}" ]; then
  if ! curl --fail --silent --show-error --connect-timeout 2 --max-time 5 \
      "${SCCACHE_ENDPOINT%/}/minio/health/live" >/dev/null 2>&1; then
    echo 'Shared compiler cache unavailable; using local cache for this job.' >&2
    unset SCCACHE_BUCKET SCCACHE_ENDPOINT SCCACHE_REGION SCCACHE_S3_USE_SSL SCCACHE_S3_KEY_PREFIX
    unset AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY AWS_SESSION_TOKEN
  fi
fi

start_ci_sccache() {
  if ! sccache --start-server >/tmp/sccache-start.log 2>&1; then
    echo 'Compiler cache startup failed; retrying with local storage.' >&2
    sccache --stop-server >/dev/null 2>&1 || true
    unset SCCACHE_BUCKET SCCACHE_ENDPOINT SCCACHE_REGION SCCACHE_S3_USE_SSL SCCACHE_S3_KEY_PREFIX
    unset AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY AWS_SESSION_TOKEN
    if ! sccache --start-server >/tmp/sccache-local-start.log 2>&1; then
      echo 'Local compiler cache unavailable; compiling without the wrapper.' >&2
      unset RUSTC_WRAPPER
    fi
  fi
}
