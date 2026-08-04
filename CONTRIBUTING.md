# Contributing to AgentStateGraph

Thank you for your interest in AgentStateGraph! This project is building a new infrastructure primitive for the AI-native era, and contributions are welcome.

## How this project is developed

AgentStateGraph is developed on a private GitLab instance and **mirrored,
read-only, to GitHub**. GitHub is the public home — it's where you file issues
and open pull requests, and it always reflects the current `main` and release
tags — but the canonical history lives on GitLab.

One consequence matters for contributors: **GitHub's `main` is force-advanced
from GitLab on every change, so pull requests are never merged with the GitHub
"Merge" button** (that would be overwritten on the next sync). Instead, accepted
changes are applied on the GitLab side by the project owner and then re-published
to GitHub. Your commits and authorship are preserved, and the PR is closed with a
link to the landed commit. If your merge doesn't come from the GitHub button,
that's the mirror model working — not a rejection.

## Getting Started

1. **Fork and clone** the repository
2. **Install Rust**: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
3. **Run tests**: `cargo test`
4. **Run an example**: `cargo run --example getting_started -p agentstategraph`

## Project Structure

```
AgentStateGraph/
├── spec/AGENTSTATEGRAPH-RFC.md     # The specification (read this first!)
├── crates/
│   ├── agentstategraph-core/       # Types, diff, merge, schema — zero I/O
│   ├── agentstategraph-storage/    # Pluggable backends (memory, SQLite, IndexedDB)
│   ├── agentstategraph/            # High-level Repository API
│   ├── agentstategraph-mcp/        # MCP server (66 tools) + HTTP + migrate CLI
│   ├── agentstategraph-tasks/      # Shared Plan/Task store — state machine, proofs, assignment
│   ├── agentstategraph-policy/     # Authorization + cost-of-change gating with fallback actions
│   ├── agentstategraph-policy-sign/ # Ed25519 signing for policy ratification
│   ├── agentstategraph-policy-wasm/ # WASM host runner for policy evaluation (stub)
│   ├── agentstategraph-taint/      # Taint/quarantine/watch mark-and-sweep primitive
│   ├── agentstategraph-reminders/  # Pull-based reminders: priority, schedules, soft refs, autonomous flag
│   ├── agentstategraph-migrate/    # Schema-evolution framework + migration registry
│   ├── agentstategraph-ffi/        # C ABI for language bindings
│   └── agentstategraph-wasm/       # Browser/Deno WASM build
├── bindings/
│   ├── python/                     # PyO3 bindings
│   ├── typescript/                 # napi-rs bindings
│   ├── go/                         # CGo bindings
│   └── swift/                      # Swift Package (macOS + iOS) over the C ABI
└── examples/                       # Reference implementations
```

## How to Contribute

### Good First Issues

Look for issues labeled `good-first-issue`. These are designed to be approachable for new contributors:

- **Add a new example**: Write a reference implementation for a specific use case
- **Improve error messages**: Make error types more descriptive
- **Add tests**: Increase coverage for edge cases in diff, merge, or query
- **Documentation**: Improve doc comments or add usage examples to doc tests

### Medium Issues

- **Schema merge hints in merge engine**: The schema system defines merge hints (`sum`, `max`, `union-by-id`, etc.) but the merge engine doesn't use them yet. Wire them together.
- **Bisect operation**: Implement binary search over the commit DAG to find where a condition changed (spec section 4.4.4).
- **Intent tree traversal**: Build the `intent_tree()` operation that returns the full decomposition tree of parent/child intents.

### Larger Contributions

- **New storage backend**: Implement `ObjectStore + CommitStore + RefStore` for a new backend (Redis, DynamoDB, etc.)
- **New language binding**: Add bindings for Ruby, Java, C#, or another language using the FFI crate
- **MCP resources**: Add MCP resource endpoints (`agentstategraph://state/{ref}/{path}`, etc.)

## Development Workflow

1. Create a branch: `git checkout -b feature/my-change`
2. Make changes
3. Run tests: `cargo test`
4. Run formatter: `cargo fmt`
5. Run clippy: `cargo clippy`
6. Commit with a clear message describing what and why
7. Open a pull request against `main` on GitHub

**Review and merge.** A maintainer reviews the PR. All changes are merged by the
**project owner**, who applies the change on GitLab; the mirror then brings it to
GitHub and the PR is closed as landed. Keep PRs small and single-purpose — they
review and land far more easily than large, multi-concern branches.

## Maintainer & agent workflow (GitLab origin)

This section is for those with write access to the canonical GitLab instance —
maintainers and the automated agents that develop this project. Public
contributors use the GitHub PR flow above.

**Feature work always lands via a merge request — never a local merge to
`main`.**

1. Branch off `main` (a git worktree is fine) and commit incrementally.
2. Push and open an MR with the [`glab`](https://gitlab.com/gitlab-org/cli) CLI:
   ```sh
   git push -u origin my-branch
   glab mr create --fill --target-branch main --remove-source-branch
   ```
3. Merge through the MR (`glab mr merge <id>`), so origin history reflects
   review. Merge requests get a detached CI pipeline (fmt, clippy, build, test)
   that must be green before merge.
4. **Traceability:** record the MR URL as the completion evidence for the unit
   of work (e.g. in the closing summary of the plan/branch it belongs to), so
   every landed change traces back to its reviewed MR.

**Cutting a release** is the one exception — it goes straight to `main`, never
through an MR (routing a version bump through review invites tag/commit-SHA
drift). There is exactly one place a human edits the version: the workspace.
[`scripts/release.sh`](scripts/release.sh) propagates it everywhere the publish
pipeline reads a version and stamps the changelog:

```sh
scripts/release.sh X.Y.Z
git commit -am "release: vX.Y.Z"
git tag -a vX.Y.Z -m "release: vX.Y.Z"    # annotated — the deploy trigger
git push --follow-tags origin main
```

The **tag** is the deploy trigger: a `version-guard` job fails the pipeline if
any version disagrees with the tag before anything publishes, then the release
artifacts build and the mirror publishes `main` and the tag to GitHub.

## Licensing of contributions

AgentStateGraph is dual-licensed under **MIT OR Apache-2.0**. Unless you state
otherwise, any contribution you intentionally submit for inclusion is offered
under those same terms, per the Apache-2.0 license's contribution clause — no
separate agreement required. See [LICENSING.md](LICENSING.md).

## Code Style

- Follow standard Rust conventions
- Write doc comments for all public items
- Add tests for new functionality
- Keep agentstategraph-core free of I/O dependencies

## Architecture Principles

- **Intent metadata is mandatory**: Every state change must have intent. Don't add write operations that skip intent.
- **Provenance is permanent**: Don't add operations that destroy history without explicit epoch/archive semantics.
- **Storage is pluggable**: Don't hard-code storage backends. All storage goes through traits.
- **Schema is optional**: AgentStateGraph must work without schemas. Schema features are additive.

## Naming

When writing prose or new docs, always use the full **AgentStateGraph**, never the short form "StateGraph" alone. The short form collides with both LangGraph's `StateGraph` class and Terrateam's Stategraph Terraform backend, and those collisions are actively harmful for our target audience. See `site/src/content/docs/compare.md` for the disambiguation page. Do not adopt "ASG" as an abbreviation — it collides with AWS Auto Scaling Groups.

## Questions?

Open an issue or start a discussion. We're building the infrastructure primitive for intent-based systems — your perspective matters.
