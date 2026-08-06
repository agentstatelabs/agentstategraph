# AgentStateGraph — Swift binding (macOS + iOS)

An AI-native, versioned, intent-carrying state store. This is the Swift
binding over the native Rust implementation via the stable C ABI
(`agentstategraph-ffi`), packaged as a Swift Package for macOS and iOS.

It provides the established cross-language surface plus the advanced native
repository contract required by branch-aware applications:

- **`AgentStateGraph`** — repository: `get` / `set` / `delete`, branches,
  `diff` / `merge`, `log`, `blame`, plus taint & migrate (below).
- **`TaskStore`** — plans and tasks with proof-gated completion, blockers,
  assignment, and `nextTask` scheduling.
- **`PolicyStore`** — propose / ratify / supersede, evaluate & change-cost
  evaluation, tenant-scoped variants, signing envelopes.
- **Taint / Quarantine / Watch** — protective markers on paths.
- **Migrate** — schema check / run.
- **Advanced repository** — namespaces, expected-head CAS, merge base and
  preview, state exploration/search, commit queries, atomic speculation,
  durable sessions, and epochs.

Binding capability status is tracked in `../capabilities.json`; see
`../../docs/BINDING_RELEASE_POLICY.md`. Bindings share a release version but do
not gain new Core APIs automatically.

All calls are `throws`; results are decoded into `Codable` Swift types.

## Remote installation (recommended)

Released versions are consumable directly from the repository root:

```swift
.package(
    url: "https://github.com/agentstatelabs/agentstategraph.git",
    from: "0.9.21"
)
```

In Xcode, choose File ▸ Add Package Dependencies and enter the same repository
URL. SwiftPM downloads the checksum-pinned release XCFramework automatically;
consumers do not build Rust or generate native artifacts.

## Local development build modes

The native Rust library is linked one of two ways, chosen at configure
time. Both expose the same `CAgentStateGraph` Clang module, so the Swift
source is identical.

### 1. XCFramework — required for real iOS devices (default)

Build a fat framework carrying static slices for macOS (arm64 + x86_64),
iOS device (arm64), and iOS simulator (arm64 + x86_64):

```sh
# from the repo root — installs rustup targets as needed
scripts/build-swift-xcframework.sh
# → bindings/swift/artifacts/AgentStateGraphFFI.xcframework
```

Then add the package (`bindings/swift`) to your app in Xcode
(File ▸ Add Packages ▸ Add Local…) or via `Package.swift`:

```swift
.package(path: "../AgentStateGraph/bindings/swift")
```

This mode links and runs on physical iPhones/iPads.

### 2. Local dylib — fast macOS / simulator dev

Skip cross-compilation and link the host build directly:

```sh
# from the repo root
cargo build --release -p agentstategraph-ffi
# then build/test the package with the local flag set:
cd bindings/swift
AGENTSTATEGRAPH_SWIFT_LOCAL=1 swift test
```

`AGENTSTATEGRAPH_SWIFT_LOCAL=1` switches the package to a `systemLibrary`
target that links `target/release/libagentstategraph_ffi`. Great for
macOS and the Simulator; it will **not** run on a device.

## Quick start

```swift
import AgentStateGraph

let asg = try AgentStateGraph()                    // in-memory; or AgentStateGraph(path:)
defer { asg.close() }

try asg.set("/name", json: "\"pico-cluster\"", category: .checkpoint, description: "init")
let name = try asg.get("/name")                    // "\"pico-cluster\""

// Typed values via Codable
struct Node: Codable { let host: String; let cores: Int }
try asg.set("/nodes/pico1", value: Node(host: "pico1", cores: 4),
            category: .checkpoint, description: "add node")
let node = try asg.get("/nodes/pico1", as: Node.self)

// Tasks
let tasks = try TaskStore(asg, prefix: "/tasks", agentId: "builder")
try tasks.createPlan("launch")
let t = try tasks.addTask(plan: "launch", title: "cut release", priority: .high)
_ = try tasks.startTask(plan: "launch", id: t.id)
_ = try tasks.completeTask(plan: "launch", id: t.id,
                           proof: Proof(kind: .commit, value: "abc123"))
```

## Memory & threading

- `AgentStateGraph`, `TaskStore`, and `PolicyStore` own native handles.
  Call `close()` when done, or rely on `deinit`. Using a handle after
  `close()` throws `.closed`.
- `TaskStore` / `PolicyStore` share (refcount) the repository; closing one
  does not close the `AgentStateGraph`.

## Signing note

Ed25519 policy signing/verification are available through the Rust API and
the MCP server. Registering a signer through the C ABI is not yet wired up,
so `sign` / `verify` return the FFI's raw JSON envelope; policies can still
be proposed, ratified, evaluated, and audited.
