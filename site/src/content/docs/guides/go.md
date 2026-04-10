---
title: Go
description: Use AgentStateGraph from Go via C FFI bindings.
---

## Prerequisites

- Go 1.21+
- Rust toolchain (to build the FFI library)
- CGo enabled

## Build the FFI Library

```bash
cd /path/to/AgentStateGraph
cargo build --release -p agentstategraph-ffi
```

This produces `target/release/libagentstategraph_ffi.a` (static) and `libagentstategraph_ffi.so`/`.dylib` (dynamic).

## Link in Your Go Project

The Go bindings live in `bindings/go/`. Copy or symlink the package into your project, or use a Go workspace.

The CGo directive in `agentstategraph.go` expects the library at `../../target/release/`:

```go
// #cgo LDFLAGS: -L${SRCDIR}/../../target/release -lagentstategraph_ffi
```

Adjust the path if your layout differs.

## Basic Operations

```go
package main

import (
    "fmt"
    "agentstategraph"
)

func main() {
    // In-memory (ephemeral)
    sg, err := agentstategraph.NewMemory()
    if err != nil {
        panic(err)
    }
    defer sg.Close()

    // Or SQLite (durable)
    // sg, err := agentstategraph.NewSQLite("state.db")

    // Set a value — every write is an atomic commit with intent
    sg.Set("/cluster/name", `"prod"`, "Checkpoint", "Initialize cluster")

    // Read it back
    val, _ := sg.Get("/cluster/name")
    fmt.Println(val)  // "prod"

    // Set structured data (pass JSON string)
    sg.Set("/cluster/network", `{"subnet":"10.0.0.0/16","dns":"1.1.1.1"}`,
        "Checkpoint", "Configure network")

    // Or use SetJSON with a Go value
    sg.SetJSON("/cluster/replicas", 3, "Refine", "Scale to 3 replicas")

    // Delete
    sg.Delete("/cluster/network", "Fix", "Remove network config")
}
```

## Branches and Merge

```go
// Create a branch
sg.Branch("feature/new-network", "main")

// Write to it
sg.Set("/cluster/network", `"flannel"`, "Explore", "Try flannel",
    "feature/new-network")

// Diff
diff, _ := sg.Diff("main", "feature/new-network")
fmt.Println(diff)  // JSON array of structured DiffOps

// Merge
sg.Merge("feature/new-network", "main", "Adopt flannel")
```

## Log and Blame

```go
// Commit log (returns JSON)
log, _ := sg.Log(5)
fmt.Println(log)

// Log from a specific branch
log, _ = sg.Log(10, "feature/new-network")

// Blame — who modified a path and why
blame, _ := sg.Blame("/cluster/replicas")
fmt.Println(blame)
```

## Write to a Non-Main Branch

All read/write functions accept an optional trailing ref argument:

```go
// Set on a feature branch
sg.Set("/app/version", `"2.0"`, "Explore", "Try v2", "feature/v2")

// Get from a feature branch
val, _ := sg.Get("/app/version", "feature/v2")
```

## Available Functions

| Function | Description |
|----------|-------------|
| `NewMemory()` | Create in-memory store |
| `NewSQLite(path)` | Create SQLite-backed store |
| `Close()` | Free the repository handle |
| `Get(path, ...ref)` | Read JSON value at path |
| `Set(path, json, category, description, ...ref)` | Write with intent |
| `SetJSON(path, value, category, description, ...ref)` | Write Go value as JSON |
| `Delete(path, category, description, ...ref)` | Delete with intent |
| `Branch(name, from)` | Create a branch |
| `Diff(refA, refB)` | Structured diff (JSON) |
| `Merge(source, target, description)` | Merge branches |
| `Log(limit, ...ref)` | Commit history (JSON) |
| `Blame(path, ...ref)` | Who modified a path and why |

## Static Linking

For static linking (no runtime dependency on the `.so`/.`dylib`):

```go
// #cgo LDFLAGS: -L${SRCDIR}/../../target/release -lagentstategraph_ffi -lm -ldl -lpthread
```

Build with:

```bash
CGO_ENABLED=1 go build -ldflags '-extldflags "-static"' .
```
