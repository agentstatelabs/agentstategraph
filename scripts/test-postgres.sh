#!/usr/bin/env bash
#
# test-postgres — run the Postgres legs of the backend conformance suites
# (DESIGN.md Plan D t-001) against a throwaway Postgres, the way CI does.
#
#   scripts/test-postgres.sh                 # the Postgres-relevant test targets
#   scripts/test-postgres.sh --workspace     # the whole workspace test suite
#   scripts/test-postgres.sh -p <crate> ...  # any `cargo test` arguments
#
# With TEST_DATABASE_URL already set, that database is used and nothing is
# started. Otherwise a Postgres 16 is started for the run and removed after:
# in Docker when a daemon is running, else from a local `initdb`/`pg_ctl`
# (Homebrew `postgresql@16`, or apt `postgresql`). Each test uses its own
# tenant, so a shared database is fine.
#
# Portable to bash 3.2 (macOS default).

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

CLEANUP=""
cleanup() { if [ -n "$CLEANUP" ]; then eval "$CLEANUP" >/dev/null 2>&1 || true; fi; }
trap cleanup EXIT INT TERM

free_port() {
  python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()'
}

start_docker() {
  local port name
  port="$(free_port)"
  name="asg-test-postgres-$$"
  docker run -d --rm --name "$name" \
    -e POSTGRES_USER=asg -e POSTGRES_PASSWORD=asg -e POSTGRES_DB=asg_test \
    -p "127.0.0.1:${port}:5432" postgres:16 >/dev/null
  CLEANUP="docker rm -f $name"
  for _ in $(seq 1 60); do
    if docker exec "$name" pg_isready -U asg -d asg_test >/dev/null 2>&1; then
      export TEST_DATABASE_URL="postgres://asg:asg@127.0.0.1:${port}/asg_test"
      return 0
    fi
    sleep 1
  done
  echo "test-postgres: Postgres in Docker did not become ready" >&2
  exit 1
}

start_local() {
  local port dir
  port="$(free_port)"
  # A short path: Postgres caps Unix socket paths at ~103 bytes, so sockets
  # are disabled below and only TCP is used.
  dir="$(mktemp -d /tmp/asg-pg.XXXXXX)"
  initdb -D "$dir/data" -U asg --auth=trust -E UTF8 >/dev/null
  {
    echo "listen_addresses = '127.0.0.1'"
    echo "port = $port"
    echo "unix_socket_directories = ''"
  } >>"$dir/data/postgresql.conf"
  # macOS: without a valid locale the postmaster refuses to start
  # ("postmaster became multithreaded during startup").
  LC_ALL="${LC_ALL:-en_US.UTF-8}" LANG="${LANG:-en_US.UTF-8}" \
    pg_ctl -D "$dir/data" -l "$dir/log" -w start >/dev/null
  CLEANUP="pg_ctl -D '$dir/data' -m immediate stop; rm -rf '$dir'"
  createdb -h 127.0.0.1 -p "$port" -U asg asg_test
  export TEST_DATABASE_URL="postgres://asg@127.0.0.1:${port}/asg_test"
}

if [ -z "${TEST_DATABASE_URL:-}" ]; then
  if command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
    start_docker
  elif command -v initdb >/dev/null 2>&1 && command -v pg_ctl >/dev/null 2>&1; then
    start_local
  else
    echo "test-postgres: need a running Docker daemon or a local Postgres (initdb, pg_ctl)," >&2
    echo "or set TEST_DATABASE_URL to an existing database." >&2
    exit 2
  fi
fi
echo "test-postgres: TEST_DATABASE_URL=$TEST_DATABASE_URL"

if [ "$#" -eq 0 ]; then
  cargo test -p agentstategraph-storage --features postgres,memory \
    --test conformance --test postgres_tenant_isolation --test postgres_taint
  cargo test -p agentstategraph --test backend_conformance
elif [ "$1" = "--workspace" ]; then
  cargo test --workspace --locked --exclude agentstategraph-python
else
  cargo test "$@"
fi
