---
title: Namespaces
description: Separate sets of branches for projects or teams sharing one store.
---

A **namespace** is a separate set of branches in one store. Branches in namespace `acme` and namespace `globex` can share names (`main`, `release`) without colliding, and listing or reading branches in one namespace never shows the other's. One server and one database can hold many projects this way.

Namespaces partition **branches**. They are not a security boundary on their own: see [What a namespace doesn't separate](#what-a-namespace-doesnt-separate), and [Confining a client to one namespace](#confining-a-client-to-one-namespace) for how to keep a client inside one.

## What a namespace is

`Namespace` is a validated newtype: ASCII alphanumerics plus `-` and `_`, 1–64 bytes, with a reserved `"default"` constant. Every ref and branch is keyed on a composite **`(namespace, name)` primary key** on the refs table, in all four storage backends (in-memory, SQLite, Postgres, IndexedDB). The partition is in the schema, not a naming convention.

## What a namespace doesn't separate

A store keeps one pool of objects and commits, shared by every namespace. Content addressing deduplicates identical data across namespaces, and a commit id names the same commit whichever namespace you ask from. So:

- **Commit ids reach across namespaces.** Anywhere a ref is accepted, a commit id or id prefix also works, and it resolves over every commit in the store. A caller who can name another namespace's commit can read that state.
- **The MCP server trusts its client.** Its tools take a per-call `namespace`, so an MCP client can work in any namespace.
- **Some operations cover the whole store:** GC (dry run, sweep, vacuum), the history report, sessions, taints and reminders.

## Confining a client to one namespace

Over HTTP, give the client an API key bound to the namespace: `namespace_id` when an admin creates it (`POST /api/admin/keys`). Requests with that key:

- read and write only that namespace;
- name refs by **branch only** — commit ids and prefixes get `403`, because they would reach other namespaces;
- can't use the store-wide endpoints (`/api/gc/*`, `/api/history`), which get `403`.

In the library, the same limit is `Repository::fork_namespace(ns).with_branch_refs_only()`.

## Tenants

A **tenant** is a separate store. On Postgres, tenants share a database and every row carries its tenant id, so each tenant's objects, commits and branches are its own. A server process serves one tenant (`--tenant`, default `default`), and a non-admin API key for another tenant is refused. On SQLite, a store is a file.

A tenant has many namespaces; a namespace belongs to one tenant. Sessions also carry a `scope_tenant`, which filters which policies apply; it doesn't choose where data lives.

## Configuring the namespace

Resolution order, most specific first: the active session's `scope_namespace` → the repository default → `"default"`.

- **Repository default:** `Repository::with_namespace(ns)` at construction, or the `--namespace` flag / `ASG_NAMESPACE` env var on the MCP server. `Repository::init()` auto-creates its configured namespace so startup never fails on a missing one.
- **Per session:** set `scope_namespace` via `CreateSessionParams`; it overrides the repository default and is cached on `set_active_session()` so ref operations don't pay a storage round-trip.
- **Per call:** 17 MCP tools and 9 WASM methods accept an optional `namespace` field that overrides the configured namespace for that one call. Omit it to use the default.
- **Per API key (HTTP):** a key with a `namespace_id` works in that namespace only, as above.

## Lifecycle and cross-namespace merge

`create_namespace` (idempotent), `list_namespaces`, and `delete_namespace` manage the set. Deleting a namespace cascades to all of its refs and refuses to remove `"default"`.

`cross_namespace_merge`, which merges a branch from another namespace into the current one, is **always refused** for now (`CrossNamespaceAccessDenied`): the policy check that would allow it isn't built yet. Same-namespace merges are plain merges.

`Repository::fork_namespace(ns)` returns a lightweight sibling that shares the same storage but works in another namespace, with its own in-memory speculation and watch state.

## MCP tools

| Tool | Purpose |
|------|---------|
| `agentstategraph_create_namespace` | Create a namespace (idempotent) |
| `agentstategraph_list_namespaces` | List all namespaces |
| `agentstategraph_delete_namespace` | Delete a namespace and its refs (not `default`) |
| `agentstategraph_cross_namespace_merge` | Merge across namespaces (refused for now; see above) |

The 17 ref-touching tools (`get`, `set`, `delete`, `branch`, `list_branches`, `merge`, `log`, `diff`, `speculate`, `query`, `blame`, `list_paths`, `get_tree`, `search`, `stats`, `commit_graph`, `intent_tree`) additionally accept an optional `namespace` override. `create_session` accepts a `namespace_id`.
