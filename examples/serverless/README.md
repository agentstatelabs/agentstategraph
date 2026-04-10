# AgentStateGraph in Serverless/Edge Functions

AgentStateGraph compiles to WASM, which means it runs anywhere WASM runs:
- **Cloudflare Workers** — edge functions worldwide
- **Deno Deploy** — serverless TypeScript
- **AWS Lambda** (with WASM runtime)
- **Vercel Edge Functions**
- **Browsers** — client-side state management

## Why This Matters

An AI agent running as a serverless function can use AgentStateGraph to:
1. **Track its work** — every state change has intent, reasoning, and provenance
2. **Branch and explore** — speculate on approaches without committing
3. **Report back** — structured resolution with deviations
4. **Audit trail** — every function invocation is traceable through the state graph

Traditional serverless functions are stateless. AgentStateGraph gives them **structured, versioned, intent-aware state** without an external database.

## Architecture

```
User Request
    ↓
Edge Function (Cloudflare Worker / Deno Deploy)
    ↓
AgentStateGraph (WASM, ~1MB)
    ├── In-memory state (fast, per-invocation)
    ├── IndexedDB (browser persistence)
    └── Or external KV store for cross-invocation persistence
```

## Deno Example

```typescript
// deno run --allow-net serverless_agent.ts

import init, { WasmAgentStateGraph } from "./agentstategraph_wasm.js";

await init();

// Create a AgentStateGraph instance (in-memory for serverless)
const sg = new WasmAgentStateGraph();

// Simulate an agent processing a request
function handleRequest(request: { intent: string; params: any }) {
  // Set initial state
  sg.set("/request/intent", JSON.stringify(request.intent), "Checkpoint", "Incoming request");
  sg.set("/request/params", JSON.stringify(request.params), "Checkpoint", "Request parameters");

  // Agent explores approaches
  const spec1 = sg.speculate(undefined, "Approach A");
  const spec2 = sg.speculate(undefined, "Approach B");

  sg.specSet(spec1, "/result/approach", '"cached-lookup"');
  sg.specSet(spec2, "/result/approach", '"fresh-computation"');

  // Pick the best approach
  sg.commitSpeculation(spec1, "Refine", "Cached lookup is faster for this query", undefined, 0.9);
  sg.discardSpeculation(spec2);

  // Return the result with full provenance
  const log = JSON.parse(sg.log());
  return {
    result: JSON.parse(sg.get("/result/approach")),
    provenance: log.map(c => ({
      intent: c.intent.description,
      reasoning: c.reasoning,
      confidence: c.confidence,
    })),
  };
}

// Example invocation
const response = handleRequest({
  intent: "lookup user profile",
  params: { userId: "u123" },
});

console.log("Response:", JSON.stringify(response, null, 2));
```

## Cloudflare Worker Example

```javascript
// wrangler.toml:
//   [build]
//   command = "wasm-pack build --target bundler"

import init, { WasmAgentStateGraph } from "./agentstategraph_wasm.js";

let initialized = false;

export default {
  async fetch(request, env) {
    if (!initialized) {
      await init();
      initialized = true;
    }

    const sg = new WasmAgentStateGraph();
    const url = new URL(request.url);

    // Agent processes the request with full provenance
    sg.set("/request/path", JSON.stringify(url.pathname), "Checkpoint", "Incoming request");
    sg.set("/request/method", JSON.stringify(request.method), "Checkpoint", "HTTP method");

    // Process with intent tracking
    sg.set("/response/status", "200", "Refine", "Request processed successfully",
      undefined, "Edge function determined response based on path routing", 0.95);

    // Return response with audit trail
    const log = JSON.parse(sg.log());
    return new Response(JSON.stringify({
      message: "Processed with AgentStateGraph provenance",
      audit: log.length + " state transitions tracked",
    }), {
      headers: { "Content-Type": "application/json" },
    });
  },
};
```

## Key Benefits for Serverless

| Traditional Serverless | With AgentStateGraph |
|---|---|
| Stateless — no memory between invocations | Structured state with history |
| Logging via console.log | Intent-aware provenance trail |
| Debug by reading log files | Query by intent, agent, reasoning |
| No branching/exploration | Speculate, compare, pick winner |
| "What happened?" → dig through CloudWatch | `sg.blame("/result")` → instant answer |
