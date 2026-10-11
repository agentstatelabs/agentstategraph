# agentstategraph-wasm

WASM bindings for AgentStateGraph, for browsers, Deno and Node, built with
`wasm-bindgen`. Persistence in the browser goes through IndexedDB.

```js
import init, { WasmAgentStateGraph } from './agentstategraph_wasm.js'
await init()
const asg = new WasmAgentStateGraph()
asg.set("/name", "my-cluster", "Checkpoint", "init")
asg.get("/name")  // → '"my-cluster"'
```

Part of [AgentStateGraph](https://github.com/agentstatelabs/agentstategraph). Most applications depend on the
[`agentstategraph`](https://crates.io/crates/agentstategraph) crate, which
re-exports the core types and storage backends.

## License

Licensed under either of
[Apache License, Version 2.0](https://github.com/agentstatelabs/agentstategraph/blob/main/LICENSE-APACHE)
or [MIT license](https://github.com/agentstatelabs/agentstategraph/blob/main/LICENSE-MIT),
at your option.
