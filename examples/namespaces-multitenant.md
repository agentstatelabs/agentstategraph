# Example: Per-project branches with namespaces

This walkthrough shows how one AgentStateGraph store holds two projects
(`acme` and `globex`), each with its own branches, what namespaces don't
separate, and how to confine a client to one of them.

All snippets are MCP tool calls — paste them into any MCP-connected agent, or
translate the tool name + arguments to the HTTP/library equivalent. Namespaces
are a server/library and MCP feature; the Python/TypeScript bindings do not yet
expose them.

Start the server with a default namespace:

```bash
agentstategraph-mcp --namespace acme
# or: ASG_NAMESPACE=acme agentstategraph-mcp
```

## 1. Create the two namespaces

```json
// agentstategraph_create_namespace
{ "name": "acme" }
```
```json
// agentstategraph_create_namespace
{ "name": "globex" }
```

```json
// agentstategraph_list_namespaces
{}
// → ["acme", "default", "globex"]
```

## 2. Same branch name, two namespaces, zero collision

Write a `main`-branch value for each tenant. The per-call `namespace` field
overrides the server default for that one call.

```json
// agentstategraph_set
{
  "path": "/billing/plan",
  "value": "enterprise",
  "intent_category": "Checkpoint",
  "intent_description": "Acme is on the enterprise plan",
  "namespace": "acme"
}
```
```json
// agentstategraph_set
{
  "path": "/billing/plan",
  "value": "starter",
  "intent_category": "Checkpoint",
  "intent_description": "Globex is on the starter plan",
  "namespace": "globex"
}
```

Reading the same path in each namespace returns each project's own value,
because each namespace has its own `main`:

```json
// agentstategraph_get
{ "path": "/billing/plan", "namespace": "acme" }   // → "enterprise"
```
```json
// agentstategraph_get
{ "path": "/billing/plan", "namespace": "globex" } // → "starter"
```

## 3. Cross-namespace merge is refused

Merging a branch from another namespace is refused for now: the policy check
that would allow it isn't built yet.

```json
// agentstategraph_cross_namespace_merge
{
  "source_namespace": "acme",
  "source_branch": "shared/templates",
  "target_branch": "main",
  "intent_description": "Reuse Acme's onboarding templates for Globex"
}
// → Error: cross-namespace merge denied: no PolicyStore configured
```

Same-namespace merges (`agentstategraph_merge`) are plain merges.

## 4. What namespaces don't separate

Both namespaces share the store's objects and commits. A commit id names the
same commit from any namespace, so a caller who knows one of Acme's commit ids
can read it while working in `globex`. MCP tools take a per-call `namespace`,
so an MCP client can work in any namespace: the MCP server trusts its client.

To confine a client to one namespace, serve it over HTTP with an API key bound
to that namespace (`namespace_id` when an admin creates the key). That key
reads and writes only its namespace, names refs by branch only (commit ids get
`403`), and can't use the store-wide GC and history endpoints.

## Key takeaways

- One store, one set of branches per namespace: branches are keyed on a
  composite `(namespace, name)` primary key.
- The per-call `namespace` field works on all 17 ref-touching tools.
- Namespaces partition branches; they aren't a security boundary. An API key
  bound to a namespace is what confines a client.
- Cross-namespace merge is refused for now.

See the [Namespaces guide](https://agentstategraph.dev/guides/namespaces/).
