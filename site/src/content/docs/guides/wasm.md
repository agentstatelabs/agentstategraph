---
title: WebAssembly
description: Run StateGraph in the browser, Deno, or serverless/edge runtimes via WASM.
---

## Build with wasm-pack

```bash
# Install wasm-pack if needed
cargo install wasm-pack

# Build the WASM package
cd /path/to/StateGraph
wasm-pack build crates/stategraph-wasm --target web --out-dir pkg
```

This produces `pkg/` with:
- `stategraph_wasm.js` — JS glue code
- `stategraph_wasm_bg.wasm` — the WASM binary
- `stategraph_wasm.d.ts` — TypeScript types

For bundler targets (webpack, vite):

```bash
wasm-pack build crates/stategraph-wasm --target bundler --out-dir pkg
```

For Node.js:

```bash
wasm-pack build crates/stategraph-wasm --target nodejs --out-dir pkg
```

## Browser Usage

```html
<script type="module">
import init, { WasmStateGraph } from './pkg/stategraph_wasm.js'

await init()
const sg = new WasmStateGraph()

// Set a value
sg.set("/app/name", '"my-app"', "Checkpoint", "Init")

// Read it back
const name = sg.get("/app/name")  // '"my-app"'
console.log(JSON.parse(name))     // "my-app"

// Branch, modify, diff, merge
sg.branch("feature")
sg.set("/app/version", '"2.0"', "Explore", "Try v2", "feature")
const diff = sg.diff("main", "feature")
console.log(JSON.parse(diff))
sg.merge("feature", "main", "Adopt v2")
</script>
```

All values are passed as JSON strings (stringify on write, parse on read).

## IndexedDB Persistence

The WASM build uses an in-memory store with write-through queuing to IndexedDB. Persistence requires a small JS bridge to flush pending writes.

### Startup: Load from IndexedDB

```javascript
const sg = new WasmStateGraph("my-app-state")

// Load persisted data from IndexedDB on startup
const db = await openIndexedDB(sg.dbName())
const objects = await getAllFromStore(db, "objects")
const commits = await getAllFromStore(db, "commits")
const refs = await getAllFromStore(db, "refs")

sg.loadObjects(JSON.stringify(objects))
sg.loadCommits(JSON.stringify(commits))
sg.loadRefs(JSON.stringify(refs))
```

### After Writes: Flush to IndexedDB

```javascript
function flushToIndexedDB(sg, db) {
  const pendingObjects = JSON.parse(sg.drainPendingObjects())
  const pendingCommits = JSON.parse(sg.drainPendingCommits())
  const pendingRefs = JSON.parse(sg.drainPendingRefs())

  const tx = db.transaction(
    ["objects", "commits", "refs"], "readwrite"
  )
  for (const [id, json] of pendingObjects) {
    tx.objectStore("objects").put({ id, data: json })
  }
  for (const [id, json] of pendingCommits) {
    tx.objectStore("commits").put({ id, data: json })
  }
  for (const [id, json] of pendingRefs) {
    tx.objectStore("refs").put({ id, data: json })
  }
}

// Flush after each write, or debounce
sg.set("/app/count", "1", "Checkpoint", "increment")
flushToIndexedDB(sg, db)
```

### IndexedDB Helper

```javascript
function openIndexedDB(name) {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(name, 1)
    request.onupgradeneeded = () => {
      const db = request.result
      db.createObjectStore("objects", { keyPath: "id" })
      db.createObjectStore("commits", { keyPath: "id" })
      db.createObjectStore("refs", { keyPath: "id" })
    }
    request.onsuccess = () => resolve(request.result)
    request.onerror = () => reject(request.error)
  })
}

function getAllFromStore(db, storeName) {
  return new Promise((resolve, reject) => {
    const tx = db.transaction(storeName, "readonly")
    const req = tx.objectStore(storeName).getAll()
    req.onsuccess = () =>
      resolve(req.result.map(r => [r.id, r.data]))
    req.onerror = () => reject(req.error)
  })
}
```

## Speculation in the Browser

```javascript
const specA = sg.speculate(undefined, "approach-a")
const specB = sg.speculate(undefined, "approach-b")

sg.specSet(specA, "/config/theme", '"dark"')
sg.specSet(specB, "/config/theme", '"light"')

const valA = JSON.parse(sg.specGet(specA, "/config/theme"))
const valB = JSON.parse(sg.specGet(specB, "/config/theme"))

// Pick one
sg.commitSpeculation(specA, "Checkpoint", "Use dark theme")
sg.discardSpeculation(specB)
```

## Epochs

```javascript
sg.createEpoch("session-001", "User editing session")
// ... do work ...
sg.sealEpoch("session-001", "Session complete")
const epochs = JSON.parse(sg.listEpochs())
```

## Serverless / Edge Runtimes

StateGraph WASM works in any runtime that supports `WebAssembly`:

**Cloudflare Workers:**
```javascript
import { WasmStateGraph } from './pkg/stategraph_wasm.js'

export default {
  async fetch(request) {
    const sg = new WasmStateGraph()
    sg.set("/request/path", JSON.stringify(new URL(request.url).pathname),
      "Checkpoint", "Log request")
    return new Response(sg.get("/request/path"))
  }
}
```

**Deno:**
```typescript
import init, { WasmStateGraph } from './pkg/stategraph_wasm.js'
await init()
const sg = new WasmStateGraph()
sg.set("/name", '"deno-app"', "Checkpoint", "init")
```

**Vercel Edge Functions / Netlify Edge:**
Same pattern as Cloudflare Workers. Import the WASM module and use `WasmStateGraph` directly.

For persistence in serverless, pair with Durable Objects (Cloudflare) or an external store. The in-memory state is ephemeral per invocation unless you hydrate from storage on startup.

## API Reference

| Method | Description |
|--------|-------------|
| `new WasmStateGraph(dbName?)` | Create store (optional IndexedDB name) |
| `get(path, ref?)` | Read JSON string at path |
| `set(path, json, category, description, ref?, reasoning?, confidence?)` | Write with intent |
| `delete(path, category, description, ref?)` | Delete with intent |
| `branch(name, from?)` | Create a branch |
| `merge(source, target?, description?)` | Merge branches |
| `diff(refA, refB)` | Structured diff (JSON string) |
| `log(ref?, limit?)` | Commit history (JSON string) |
| `blame(path, ref?)` | Who modified a path (JSON string) |
| `speculate(from?, label?)` | Create speculation (returns handle ID) |
| `specGet(handleId, path)` | Read from speculation |
| `specSet(handleId, path, json)` | Write in speculation |
| `commitSpeculation(handleId, category, description, reasoning?, confidence?)` | Commit speculation |
| `discardSpeculation(handleId)` | Discard speculation |
| `createEpoch(id, description)` | Create epoch |
| `sealEpoch(id, summary)` | Seal epoch |
| `listEpochs()` | List epochs (JSON string) |
| `loadObjects(json)` / `loadCommits(json)` / `loadRefs(json)` | Hydrate from IndexedDB |
| `drainPendingObjects()` / `drainPendingCommits()` / `drainPendingRefs()` | Get writes to flush |
