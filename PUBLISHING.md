# Publishing StateGraph

## crates.io (Rust)

1. Create account at https://crates.io (login with GitHub)
2. Get token from https://crates.io/me
3. Run:
```bash
cargo login <your-token>
cargo publish -p stategraph-core
cargo publish -p stategraph-storage
cargo publish -p stategraph
cargo publish -p stategraph-mcp
```

Note: Publish in order — each crate depends on the previous.

## PyPI (Python)

1. Create account at https://pypi.org
2. Get API token from https://pypi.org/manage/account/
3. Run:
```bash
cd bindings/python
source .venv/bin/activate
maturin publish --username __token__ --password <your-token>
```

This builds and uploads the wheel. Users install with: `pip install stategraph`

## npm (TypeScript/Node)

1. Create account at https://www.npmjs.com
2. Run:
```bash
cd bindings/typescript
npm login
npm publish
```

Users install with: `npm install stategraph`

## WASM (npm)

1. Install wasm-pack: `cargo install wasm-pack`
2. Run:
```bash
wasm-pack build crates/stategraph-wasm --target web --release
cd crates/stategraph-wasm/pkg
npm publish
```

Users import with:
```js
import init, { WasmStateGraph } from 'stategraph-wasm';
await init();
const sg = new WasmStateGraph();
```
