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
8. **After the release lands, bump the site strings.** Nothing derives them
   from this repo — see [RELEASE.md](../RELEASE.md).

## After the release

The marketing site carries version strings that nothing derives from this
repo. That checklist lives in [RELEASE.md](../RELEASE.md) — it is release
mechanics, not binding policy.

## Current audit — 1.1.0 (full pass)

The full step 1–6 review for 1.1.0 was completed on 2026-08-27 — a real
audit, not a re-affirmation, because 1.1.0 carries real surface changes.
`reviewed_core_version` is bumped in the `release-prep` commit itself: the
capability check requires it to equal the workspace version, so it cannot
move ahead of the version bump.

**What changed in Core.** Epochs gained an explicit `EpochScope`
(`All`/`Branch`/`Plan`/`Workspace`), a persisted `seal_hash`, and a `namespace`.
`EpochEntry` gained `scope` and `namespace`. `EpochStore::seal_epoch` takes the
seal hash — a storage-trait change, not a binding surface. `Repository`
signatures are unchanged; `create_epoch_scoped` and `list_all_epochs` are new.

**The one behavioural change that reaches bindings.** `Repository::list_epochs`
is now scoped to the active workspace, where it previously returned every epoch
in the store. That silently degraded the three bindings classifying `epochs` as
`full` while classifying `namespaces` as `unavailable` — python, typescript and
wasm — because they cannot select a workspace and so could no longer reach any
epoch outside the default one.

Rather than demote them to `partial`, each of the three now exposes
`list_all_epochs`, restoring the cross-workspace view. `epochs` therefore stays
`full` for them, honestly. rust, c and swift classify `namespaces` as `full` and
were unaffected; go and dotnet classify `epochs` as `unavailable` and have no
epoch surface to change.

No new capability group or ABI operation was added, so the manifest's capability
list and the advanced ABI contract are unchanged at 39 operations.

## Superseded audit — 0.9.21, re-affirmed for 1.0.0

`reviewed_core_version` was moved to 1.0.0 on 2026-08-24 by **re-affirmation,
not a fresh audit**: the diff between v0.9.24 and the 1.0.0 release commit was
two lines of `.gitignore`, with no changes under `bindings/`, no FFI or header
changes, and no public API changes — so the classification below still held.

The next release with real surface changes needs the full step 1–6 pass, not
another re-affirmation.


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
