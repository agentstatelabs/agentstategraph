# AgentStateGraph — C# / .NET binding

AI-native versioned state store for intent-based systems. This is the
C# binding over the native Rust implementation of AgentStateGraph,
exposed via P/Invoke over the stable C ABI.

NuGet package id: **`agentstatelabs.AgentStateGraph`**
Status: **`0.7.25-beta.1`** — initial preview. See the repo
[`CHANGELOG.md`](../../CHANGELOG.md) for what's new.

## Target frameworks

- `net10.0` — current .NET LTS (recommended)
- `net8.0` — minimum supported (.NET LTS, supported through Nov 2026)

Runs on Windows, macOS, and Linux (x64 + arm64). A native
`agentstategraph_ffi` library has to be reachable at runtime — see
[Native library loading](#native-library-loading).

## Install

Once published to NuGet:

```sh
dotnet add package agentstatelabs.AgentStateGraph --version 0.7.25-beta.1
```

The NuGet package ships the native `agentstategraph_ffi` library for
the supported runtime identifiers under `runtimes/<rid>/native/`; no
extra configuration is needed for the common case.

## Build from source

```sh
# 1. Build the Rust FFI crate (produces the native library)
cargo build -p agentstategraph-ffi --release

# 2. Build the C# binding
cd bindings/dotnet
dotnet build -c Release

# 3. Run the tests — the test host sets AGENTSTATEGRAPH_FFI_PATH
#    to the cargo target dir so the native library is found.
AGENTSTATEGRAPH_FFI_PATH=$PWD/../../target/release \
  dotnet test -c Release
```

## Use

> Note: the §1 release ships only the project skeleton and the
> native-library loader. The P/Invoke layer (§2) and the idiomatic C#
> surface (§3) land in subsequent commits on the 0.7.25-beta.1 branch.
> Once they land, usage will look like:

```csharp
using AgentStateGraph;

using var repo = Repository.OpenInMemory();
// … Repository / TaskStore / PolicyStore APIs …
```

## Native library loading

At runtime the binding needs to find the native
`agentstategraph_ffi` shared library. It is searched in this order:

1. **`AGENTSTATEGRAPH_FFI_PATH`** environment variable — if set, it
   is treated as a directory to look in. This is the explicit
   override and the recommended way to point at a non-default build
   during development.
2. **Alongside the managed assembly** — specifically the NuGet
   `runtimes/<rid>/native/` convention. This is what the published
   NuGet package uses.
3. **Cargo target directory** — a development convenience. The
   loader walks up from `AppContext.BaseDirectory` looking for a
   `target/debug/` or `target/release/` directory and loads from
   there. This lets you `dotnet run` straight out of a clone without
   setting environment variables.

The platform-specific file names the loader searches for:

| OS      | File name                       |
|---------|---------------------------------|
| Linux   | `libagentstategraph_ffi.so`     |
| macOS   | `libagentstategraph_ffi.dylib`  |
| Windows | `agentstategraph_ffi.dll`       |

If none of the three strategies finds a library, the loader falls
back to the default .NET resolver, which will search the OS library
search path (`LD_LIBRARY_PATH`, `DYLD_LIBRARY_PATH`, `PATH`, etc.).

## License

Business Source License 1.1. See [`LICENSE`](../../LICENSE) and
[`LICENSING.md`](../../LICENSING.md) in the repo root.
