# AgentStateGraph — Go binding

AI-native versioned state store for intent-based systems. This is the Go
binding over the native Rust implementation, via cgo over the stable C ABI
(`agentstategraph-ffi`).

## Prerequisite: build the native FFI library

Unlike a pure-Go module, this package links against a native library and is
**not** usable with a bare `go get` until that library exists. cgo in
[`agentstategraph.go`](agentstategraph.go) links it with:

```go
// #cgo LDFLAGS: -L${SRCDIR}/../../target/release -lagentstategraph_ffi
```

So the shared library must be built from this repository first:

```sh
# from the repo root
cargo build --release -p agentstategraph-ffi
# produces target/release/libagentstategraph_ffi.{so,dylib} (or .dll on Windows)
```

Then, from within a clone of this repo:

```sh
cd bindings/go
go test ./...
go build ./...
```

## Using it from another module

Because the library path is repo-relative, consuming the package from an
external module requires pointing the linker at your built library. Build
`agentstategraph-ffi` (above), then set the cgo flags to its location:

```sh
export CGO_LDFLAGS="-L/path/to/agentstategraph/target/release -lagentstategraph_ffi"
# and make the shared library resolvable at runtime, e.g. on Linux:
export LD_LIBRARY_PATH="/path/to/agentstategraph/target/release:$LD_LIBRARY_PATH"
# or on macOS:
export DYLD_LIBRARY_PATH="/path/to/agentstategraph/target/release:$DYLD_LIBRARY_PATH"
```

A future release will vendor prebuilt libraries per platform so the module
links without a local Rust build.

## Signing note

Ed25519 policy signing and verification are available through the Rust API
and the MCP server. Registering a signer through the C ABI is not yet wired
up, so `policy_sign` / `policy_verify` are unavailable from this binding;
policies can still be proposed, ratified, evaluated, and audited.
