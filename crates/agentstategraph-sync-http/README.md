# agentstategraph-sync-http

Push and pull AgentStateGraph stores over HTTP, the way git remotes work:
each store keeps its own copy and sends or fetches only what the other side
lacks.

- **`server` feature.** The `/sync/v1/{store}/refs`, `/fetch` and `/push`
  routes as an axum router. The host supplies a `StoreResolver` that
  authenticates each request and picks its store, so one host can serve
  many accounts behind its own tokens. `agentstategraph-mcp --hub` mounts
  it for a single store.
- **`client` feature.** `HttpRemote`, a blocking `Remote` for
  `Repository::push`, `fetch` and `pull`, over ureq with rustls.
- **The library does the checking.** Every id in a bundle is recomputed,
  pushes only move a ref forward, secrets and local-only refs never leave a
  store, and the receiving side keeps a push log. This crate only carries
  the calls.

```rust
use agentstategraph_sync_http::HttpRemote;

let hub = HttpRemote::new("https://hub.example.com", "default").with_token(api_key);
repo.pull("hub", &hub, "main", commit_options)?;
repo.push("hub", &hub, "main")?;
```

Part of [AgentStateGraph](https://github.com/agentstatelabs/agentstategraph). Most applications depend on the
[`agentstategraph`](https://crates.io/crates/agentstategraph) crate, which
re-exports the core types and storage backends.

## License

Licensed under either of
[Apache License, Version 2.0](https://github.com/agentstatelabs/agentstategraph/blob/main/LICENSE-APACHE)
or [MIT license](https://github.com/agentstatelabs/agentstategraph/blob/main/LICENSE-MIT),
at your option.
