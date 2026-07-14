# AgentStateGraph server — multi-stage build
#
# Usage:
#   docker build -t agentstategraph .
#   docker run -p 3001:3001 agentstategraph --http
#   docker run -p 3001:3001 -v ./data:/data agentstategraph --http --path /data/state.db
#
# MCP mode (stdio):
#   docker run -i agentstategraph

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

EXPOSE 3001

USER 1000:1000

ENTRYPOINT ["agentstategraph-mcp"]
CMD ["--http", "--port", "3001"]
