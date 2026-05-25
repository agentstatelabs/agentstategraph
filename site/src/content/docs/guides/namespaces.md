---
title: Namespaces
description: Ref-layer isolation for multi-project and multi-tenant deployments.
---

A **namespace** is AgentStateGraph's isolation boundary at the ref layer. One server, one database, one connection — but branches in namespace `acme` are completely invisible to namespace `globex`, and the two can hold same-named branches (`main`, `release`) without collision.

This is what lets a single AgentStateGraph deployment back many projects or many tenants without giving each one its own storage connection.

## What a namespace is

`Namespace` is a validated newtype: ASCII alphanumerics plus `-` and `_`, 1–64 bytes, with a reserved `"default"` constant. Every ref and branch is keyed on a composite **`(namespace, name)` primary key** on the refs table — and that holds across all four storage backends (in-memory, SQLite, Postgres, IndexedDB). Isolation is enforced by the storage schema, not by a naming convention you have to remember.

## Tenant vs. namespace

These are orthogonal and both kept on a session:

- **`scope_tenant`** is a *policy-evaluation filter* — it restricts which policies apply, by `tenant_id`. It answers "who" (auth/billing domain).
- **`scope_namespace`** is a *ref-storage isolation boundary* — it decides where branches physically live. It answers "what" (data partition).

A tenant has many namespaces; a namespace belongs to one tenant.

## Configuring the namespace

Resolution order, most specific first: the active session's `scope_namespace` → the repository default → `"default"`.

- **Repository default:** `Repository::with_namespace(ns)` at construction, or the `--namespace` flag / `ASG_NAMESPACE` env var on the MCP server. `Repository::init()` auto-creates its configured namespace so startup never fails on a missing one.
- **Per session:** set `scope_namespace` via `CreateSessionParams`; it overrides the repository default and is cached on `set_active_session()` so ref operations don't pay a storage round-trip.
- **Per call (v0.9.1):** 17 MCP tools and 9 WASM methods accept an optional `namespace` field that overrides the configured namespace for that one call. Omit it to use the default.
- **Per API key:** an `ApiKey` can carry a `namespace_id`, propagated into the auth context.

## Lifecycle and cross-namespace merge

`create_namespace` (idempotent), `list_namespaces`, and `delete_namespace` manage the set. Deleting a namespace cascades to all of its refs and refuses to remove `"default"`.

Crossing the boundary is deliberately hard. `cross_namespace_merge` merges a branch from a source namespace into the current one — and it is **denied by default**: without a configured `PolicyStore` it returns `CrossNamespaceAccessDenied`. Same-namespace merges are plain merges and never require a policy. Crossing a tenant/project boundary is a privileged, audited, policy-gated operation by design.

For per-request isolation without a second connection, `Repository::fork_namespace(ns)` returns a lightweight sibling that shares the same underlying storage but operates in a different namespace, with fresh in-memory speculation and watch state.

## MCP tools

| Tool | Purpose |
|------|---------|
| `agentstategraph_create_namespace` | Create a namespace (idempotent) |
| `agentstategraph_list_namespaces` | List all namespaces |
| `agentstategraph_delete_namespace` | Delete a namespace and its refs (not `default`) |
| `agentstategraph_cross_namespace_merge` | Policy-gated merge across namespaces |

The 17 ref-touching tools (`get`, `set`, `delete`, `branch`, `list_branches`, `merge`, `log`, `diff`, `speculate`, `query`, `blame`, `list_paths`, `get_tree`, `search`, `stats`, `commit_graph`, `intent_tree`) additionally accept an optional `namespace` override. `create_session` accepts a `namespace_id`.
