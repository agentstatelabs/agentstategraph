---
title: Python
description: Use StateGraph from Python via native bindings (PyO3).
---

## Install

```bash
cd bindings/python
python3 -m venv .venv && source .venv/bin/activate
pip install maturin && maturin develop --release
```

## Create a Store

```python
from agentstategraph_py import StateGraph

# In-memory (ephemeral)
sg = StateGraph()

# SQLite (durable)
sg = StateGraph("state.db")
```

## Basic CRUD

```python
# Set a value — every write is an atomic commit with intent
sg.set("/cluster/name", "prod", "Initialize cluster",
       category="Checkpoint")

# Read it back
name = sg.get("/cluster/name")  # "prod"

# Set with full provenance
sg.set("/cluster/replicas", 3, "Scale to 3 replicas",
       category="Refine",
       agent="agent/scaler",
       reasoning="Traffic increased 40% over last hour",
       confidence=0.85,
       tags=["scaling", "auto"])

# Set structured data
sg.set_json("/cluster/network", {
    "subnet": "10.0.0.0/16",
    "dns": "1.1.1.1"
}, "Configure network", category="Checkpoint")

# Delete
sg.delete("/cluster/network", "Remove network config",
          category="Fix")
```

## Branches

```python
# Create a branch
sg.branch("feature/new-network")

# Write to it
sg.set("/cluster/network", "flannel", "Try flannel",
       ref="feature/new-network", category="Explore")

# List branches
branches = sg.list_branches()               # all
branches = sg.list_branches(prefix="feature/")  # filtered

# Diff
changes = sg.diff("main", "feature/new-network")
for c in changes:
    print(c["path"], c["op"])

# Merge
sg.merge("feature/new-network", "main",
         description="Adopt flannel",
         reasoning="Lower overhead than calico")

# Delete a branch
sg.delete_branch("feature/new-network")
```

## Speculation

Lightweight, disposable branches for the "try many, pick one" pattern.

```python
# Create two speculations from main
spec_a = sg.speculate("main", label="approach-nfs")
spec_b = sg.speculate("main", label="approach-ceph")

# Modify each independently
sg.spec_set(spec_a, "/storage/type", "nfs")
sg.spec_set(spec_b, "/storage/type", "ceph")
sg.spec_set(spec_b, "/storage/replicas", 3)

# Read from a speculation
val = sg.spec_get(spec_a, "/storage/type")  # "nfs"

# Pick the winner, discard the loser
sg.commit_speculation(spec_a, "Use NFS",
                      category="Checkpoint",
                      reasoning="Only 2 nodes, Ceph needs 3+",
                      confidence=0.9)
sg.discard_speculation(spec_b)
```

## Query

Composable filters on the commit history. All filters are AND-combined.

```python
# By agent
commits = sg.query(agent_id="agent/scaler")

# By intent category
commits = sg.query(intent_category="Explore")

# By tags
commits = sg.query(tags=["scaling"])

# By reasoning text (full-text search)
commits = sg.query(reasoning_contains="traffic")

# By confidence range
commits = sg.query(confidence_min=0.8, confidence_max=1.0)

# Combined
commits = sg.query(
    agent_id="agent/scaler",
    intent_category="Refine",
    confidence_min=0.7,
    limit=5
)
```

## Blame

Find who last modified a path and why.

```python
entry = sg.blame("/cluster/replicas")
# {
#   "agent": "agent/scaler",
#   "intent": {"category": "Refine", "description": "Scale to 3 replicas"},
#   "reasoning": "Traffic increased 40% over last hour",
#   "confidence": 0.85,
#   "timestamp": "2026-04-06T..."
# }
```

## Epochs

Group related work into bounded, sealable audit bundles.

```python
# Create
sg.create_epoch("2026-04-incident", "Node3 recovery",
                root_intents=["intent-001"])

# ... do work ...

# Seal (immutable, tamper-evident Merkle root)
sg.seal_epoch("2026-04-incident", "Recovered node3, replicas restored")

# List
epochs = sg.list_epochs()
```

## Log

```python
# Recent history
log = sg.log("main", limit=5)
for entry in log:
    print(f"{entry['id']}: {entry['intent']['description']} "
          f"(by {entry['agent']}, confidence: {entry['confidence']})")
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
