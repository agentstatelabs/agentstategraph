# AgentStateGraph server — multi-stage build
#
# Usage:
#   docker build -t agentstategraph .
#   docker run -p 127.0.0.1:3001:3001 agentstategraph --http
#   docker run -p 127.0.0.1:3001:3001 -v ./data:/data agentstategraph --http --path /data/state.db
#
# The server listens on every interface in the container (ASG_BIND below),
# so a published port reaches it. Anything that can reach that port can read
# and write the store: publish it on 127.0.0.1 as above, or run with --auth
# (see README). Change the port with -e ASG_PORT=<port>.
#
# Health check (no curl in the image): agentstategraph-mcp healthcheck
#
# MCP mode (stdio): any arguments without --http, e.g.
#   docker run -i agentstategraph --path /data/state.db

FROM rust:1.96-slim AS builder

WORKDIR /build
COPY . .

RUN cargo build --release -p agentstategraph-mcp && \
    strip target/release/agentstategraph-mcp

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/agentstategraph-mcp /usr/local/bin/agentstategraph-mcp

# v3: run as non-root. `/data` is writable by the `asg` user so SQLite
# databases and keys files can be created by the container process.
RUN groupadd --gid 1000 asg \
 && useradd --uid 1000 --gid asg --shell /bin/bash --create-home asg \
 && mkdir -p /data \
 && chown -R asg:asg /data

WORKDIR /data

# Set as environment rather than CMD flags, so they survive a CMD override
# such as `docker run ... --http`. The healthcheck subcommand reads them too.
ENV ASG_BIND=0.0.0.0 \
    ASG_PORT=3001

EXPOSE 3001

USER 1000:1000

ENTRYPOINT ["agentstategraph-mcp"]
CMD ["--http"]
