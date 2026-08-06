# Binding release policy

ASG bindings are versioned with Core, but they are not generated from the Rust
API and therefore do not update automatically. Before 0.9.20 this allowed a
binding to build successfully while silently missing newer Core capabilities.

`bindings/capabilities.json` is now the release contract. It records every
supported language, the capability groups each one fully or partially exposes,
known unavailable groups, and the Core version for which the audit was done.
The classification is deliberately honest: a shared version number means
compatibility with that release, not automatic feature parity.

## Release gate

Run:

```sh
scripts/check-binding-capabilities.py
```

The check fails when:

- the workspace version changes without a fresh binding review;
- a binding or capability is omitted from the matrix;
- an advanced C ABI operation is declared without a Swift wrapper;
- the C declarations used by Go and Swift drift apart.

Both GitLab and GitHub CI run this check. A release version bump must therefore
include an updated matrix, even when the decision is that a capability remains
unavailable in a tracked binding. This prevents silent drift while allowing
language-specific support tiers to remain explicit.

## Stable native contract

Advanced native operations use three stable C symbols:

- `agentstategraph_repository_capabilities`
- `agentstategraph_fork_namespace`
- `agentstategraph_repository_call`

The call takes an operation name and JSON request and returns JSON. Swift adds
strongly typed models and methods over this ABI. Go and .NET can adopt the same
contract without adding dozens of one-off exported symbols. Direct Rust
bindings (Python, TypeScript, and WASM) still implement idiomatic wrappers, but
must classify the same capability groups.

## Checklist for every release

1. Run the capability check before changing the version.
2. Review the Core changelog and public `Repository` surface for additions or
   signature changes.
3. Add any new capability or advanced operation to the manifest and ABI.
4. Update each binding's status and tests. An explicit `unavailable` entry is
   acceptable for a tracked binding; an unreviewed omission is not.
5. Run the Rust workspace tests and each language binding suite.
6. Bump `reviewed_core_version` only after the review is complete.
7. Build and smoke-test published artifacts, including the Swift XCFramework,
   before creating the final tag.

## Current 0.9.21 audit

C and Swift now expose the ThreadWeaver-critical advanced surface: namespaces,
CAS writes, safe merge inspection, state exploration/search, commit queries,
atomic speculation, sessions, and epochs. Python and TypeScript already expose
speculation, queries, sessions, and epochs. WASM exposes speculation, sessions,
and epochs. Go and .NET remain on the older C repository surface. The matrix is
the authoritative backlog for closing those gaps.

Core's `Schema` validation type is not yet integrated into `Repository`; it is
marked partial in Rust and unavailable in the language bindings. It should not
be advertised as binding parity until Core has a repository-level schema
registration and validation lifecycle.
