#!/usr/bin/env bash
#
# test-postgres — run the Postgres legs of the backend conformance suites
# (DESIGN.md Plan D t-001) against a throwaway Postgres, the way CI does.
#
#   scripts/test-postgres.sh                 # the Postgres-relevant test targets
#   scripts/test-postgres.sh --workspace     # the whole workspace test suite
#   scripts/test-postgres.sh --tls           # TLS: a Postgres that refuses plaintext
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
  # ("postmaster became multithreaded during startup"); C exists everywhere.
  LC_ALL="${LC_ALL:-C}" LANG="${LANG:-C}" \
    pg_ctl -D "$dir/data" -l "$dir/log" -w start >/dev/null
  CLEANUP="pg_ctl -D '$dir/data' -m immediate stop; rm -rf '$dir'"
  createdb -h 127.0.0.1 -p "$port" -U asg asg_test
  export TEST_DATABASE_URL="postgres://asg@127.0.0.1:${port}/asg_test"
}

# --tls: a local Postgres with a throwaway CA, a certificate valid only for
# "localhost", and an hba that accepts TLS connections only, so a successful
# connection proves TLS was used. Needs a local initdb and openssl.
start_local_tls() {
  local port dir
  port="$(free_port)"
  dir="$(mktemp -d /tmp/asg-pgtls.XXXXXX)"
  openssl req -x509 -newkey rsa:2048 -nodes -days 1 -subj "/CN=asg test CA" \
    -keyout "$dir/ca.key" -out "$dir/ca.crt" 2>/dev/null
  openssl req -newkey rsa:2048 -nodes -subj "/CN=localhost" \
    -keyout "$dir/server.key" -out "$dir/server.csr" 2>/dev/null
  printf 'subjectAltName=DNS:localhost\n' >"$dir/san.ext"
  openssl x509 -req -in "$dir/server.csr" -CA "$dir/ca.crt" -CAkey "$dir/ca.key" \
    -CAcreateserial -days 1 -extfile "$dir/san.ext" -out "$dir/server.crt" 2>/dev/null
  chmod 600 "$dir/server.key"
  initdb -D "$dir/data" -U asg --auth=trust -E UTF8 >/dev/null
  {
    echo "listen_addresses = 'localhost'"
    echo "port = $port"
    echo "unix_socket_directories = ''"
    echo "ssl = on"
    echo "ssl_cert_file = '$dir/server.crt'"
    echo "ssl_key_file = '$dir/server.key'"
  } >>"$dir/data/postgresql.conf"
  printf 'hostssl all all 127.0.0.1/32 trust\nhostssl all all ::1/128 trust\n' \
    >"$dir/data/pg_hba.conf"
  LC_ALL="${LC_ALL:-C}" LANG="${LANG:-C}" \
    pg_ctl -D "$dir/data" -l "$dir/log" -w start >/dev/null
  CLEANUP="pg_ctl -D '$dir/data' -m immediate stop; rm -rf '$dir'"
  PGSSLMODE=require PGSSLROOTCERT="$dir/ca.crt" \
    createdb -h localhost -p "$port" -U asg asg_test
  export TEST_DATABASE_TLS_URL="postgres://asg@localhost:${port}/asg_test"
  export TEST_DATABASE_TLS_ROOTCERT="$dir/ca.crt"
}

if [ "${1:-}" = "--tls" ]; then
  if ! command -v initdb >/dev/null 2>&1 || ! command -v openssl >/dev/null 2>&1; then
    echo "test-postgres --tls: needs a local initdb and openssl" >&2
    exit 2
  fi
  start_local_tls
  echo "test-postgres: TEST_DATABASE_TLS_URL=$TEST_DATABASE_TLS_URL"
  cargo test -p agentstategraph-storage --features postgres-tls,memory --test postgres_tls
  exit 0
fi

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
    --test conformance --test postgres_runtime --test postgres_schema --test postgres_history --test postgres_gc \
    --test postgres_tenant_isolation --test postgres_for_tenant --test postgres_taint
  cargo test -p agentstategraph --test backend_conformance
  cargo test -p agentstategraph-mcp --test postgres_binary
elif [ "$1" = "--workspace" ]; then
  cargo test --workspace --locked --exclude agentstategraph-python
else
  cargo test "$@"
fi
