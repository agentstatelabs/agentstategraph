# Example: Multi-tenant isolation with namespaces

This walkthrough shows how one AgentStateGraph deployment serves two tenants
(`acme` and `globex`) whose state is completely isolated, and how a controlled,
policy-gated merge crosses the boundary.

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
// → ["default", "acme", "globex"]
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

Reading the same path in each namespace returns each tenant's own value —
neither can see the other's refs:

```json
// agentstategraph_get
{ "path": "/billing/plan", "namespace": "acme" }   // → "enterprise"
```
```json
// agentstategraph_get
{ "path": "/billing/plan", "namespace": "globex" } // → "starter"
```

## 3. Cross-namespace merge is denied by default

Pulling a branch from another namespace is a privileged operation. Without a
configured `PolicyStore` and a matching grant, it is refused:

```json
// agentstategraph_cross_namespace_merge
{
  "source_namespace": "acme",
  "source_branch": "shared/templates",
  "target_branch": "main",
  "intent_description": "Reuse Acme's onboarding templates for Globex"
}
// → Denied: cross-namespace merge requires an active PolicyStore with a matching grant
```

This is the intended default: tenant data never crosses a boundary unless an
operator has explicitly authorized it through policy. Same-namespace merges
(`agentstategraph_merge`) never require a policy.

## Key takeaways

- One deployment, one database, full isolation — branches are keyed on a
  composite `(namespace, name)` primary key.
- The per-call `namespace` field works on all 17 ref-touching tools.
- Crossing a namespace boundary is deny-by-default and policy-gated.

See the [Namespaces guide](https://agentstategraph.dev/guides/namespaces/).
