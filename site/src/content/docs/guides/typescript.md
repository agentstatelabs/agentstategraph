---
title: TypeScript / Node.js
description: Use AgentStateGraph from TypeScript and Node.js via native bindings (napi-rs).
---

## Install

```bash
cd bindings/typescript
npm install && npm run build
```

This produces a native `.node` binary for your platform.

## Create a Store

```typescript
const { AgentStateGraph } = require('agentstategraph')

// In-memory (ephemeral)
const sg = new AgentStateGraph()

// SQLite (durable)
const sg = new AgentStateGraph("state.db")
```

## Basic CRUD

```typescript
// Set a value — every write is an atomic commit with intent
sg.set("/cluster/name", "prod", "Initialize cluster",
  undefined, "Checkpoint")

// Read it back
const name = sg.get("/cluster/name")  // "prod"

// Set with full provenance
sg.set("/cluster/replicas", 3, "Scale to 3 replicas",
  undefined,     // ref (default: "main")
  "Refine",      // category
  "agent/scaler", // agent
  "Traffic increased 40% over last hour",  // reasoning
  0.85,          // confidence
  ["scaling", "auto"]  // tags
)

// Set structured data
sg.setJson("/cluster/network", {
  subnet: "10.0.0.0/16",
  dns: "1.1.1.1"
}, "Configure network", undefined, "Checkpoint")

// Delete
sg.delete("/cluster/network", "Remove network config",
  undefined, "Fix")
```

## Branches

```typescript
// Create a branch
sg.branch("feature/new-network")

// Write to it
sg.set("/cluster/network", "flannel", "Try flannel",
  "feature/new-network", "Explore")

// List branches
const all = sg.listBranches()
const filtered = sg.listBranches("feature/")

// Diff
const changes = sg.diff("main", "feature/new-network")
for (const c of changes) {
  console.log(c.path, c.op)
}

// Merge
sg.merge("feature/new-network", "main", "Adopt flannel",
  "Lower overhead than calico")

// Delete a branch
sg.deleteBranch("feature/new-network")
```

## Speculation

Lightweight, disposable branches for the "try many, pick one" pattern.

```typescript
// Create two speculations from main
const specA = sg.speculate(undefined, "approach-nfs")
const specB = sg.speculate(undefined, "approach-ceph")

// Modify each independently
sg.specSet(specA, "/storage/type", "nfs")
sg.specSet(specB, "/storage/type", "ceph")
sg.specSet(specB, "/storage/replicas", 3)

// Read from a speculation
const val = sg.specGet(specA, "/storage/type")  // "nfs"

// Pick the winner, discard the loser
sg.commitSpeculation(specA, "Use NFS",
  "Checkpoint",
  "Only 2 nodes, Ceph needs 3+",
  0.9)
sg.discardSpeculation(specB)
```

## Query

Composable filters on the commit history. All filters are AND-combined.

```typescript
// By agent
const commits = sg.query(undefined, "agent/scaler")

// By intent category
const commits = sg.query(undefined, undefined, "Explore")

// By tags
const commits = sg.query(undefined, undefined, undefined,
  ["scaling"])

// By reasoning text
const commits = sg.query(undefined, undefined, undefined,
  undefined, "traffic")

// By confidence range
const commits = sg.query(undefined, undefined, undefined,
  undefined, undefined, 0.8, 1.0)

// Combined with limit
const commits = sg.query(
  undefined,       // ref
  "agent/scaler",  // agent_id
  "Refine",        // intent_category
  undefined,       // tags
  undefined,       // reasoning_contains
  0.7,             // confidence_min
  undefined,       // confidence_max
  undefined,       // has_deviations
  5                // limit
)
```

## Blame

Find who last modified a path and why.

```typescript
const entry = sg.blame("/cluster/replicas")
// {
//   agent: "agent/scaler",
//   intent: { category: "Refine", description: "Scale to 3 replicas" },
//   reasoning: "Traffic increased 40% over last hour",
//   confidence: 0.85,
//   timestamp: "2026-04-06T..."
// }
```

## Epochs

Group related work into bounded, sealable audit bundles.

```typescript
// Create
sg.createEpoch("2026-04-incident", "Node3 recovery",
  ["intent-001"])

// ... do work ...

// Seal (immutable, tamper-evident Merkle root)
sg.sealEpoch("2026-04-incident",
  "Recovered node3, replicas restored")

// List
const epochs = sg.listEpochs()
```

## Log

```typescript
const log = sg.log(undefined, 5)
for (const entry of log) {
  console.log(`${entry.id}: ${entry.intent.description} ` +
    `(by ${entry.agent}, confidence: ${entry.confidence})`)
}
```

## Sessions

```typescript
// List all active sessions
const sessions = sg.sessions()

// Filter by agent
const sessions = sg.sessions("agent/planner")
```

## Intent Categories

| Category | Use for |
|----------|---------|
| `Checkpoint` | Saving known-good state |
| `Explore` | Trying an approach |
| `Refine` | Improving existing state |
| `Fix` | Correcting errors |
| `Rollback` | Reverting to prior state |
| `Merge` | Combining branch work |
| `Migrate` | Schema/structural changes |
