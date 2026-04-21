# agentstategraph-mcp

MCP server + HTTP REST API for AgentStateGraph. Exposes the versioned
state store, speculation engine, plan/task primitive, and policy
primitive as tools callable from any MCP client (Claude Code, GPT,
Cursor, etc.).

Run as MCP over stdio:

```
agentstategraph-mcp
```

Run as HTTP:

```
agentstategraph-mcp --http --port 3001
```

## Policy tools (Phase 3 of the 0.6 milestone)

The server binds a `PolicyStore` at `/policies` alongside the existing
`TaskStore` at `/plans`. Nine new tools cover the full policy surface
from POLICY_V1.md §§6 + §22.5:

| Tool | One-liner |
|---|---|
| `policy_propose` | Write a proposed (unratified) policy at the given path. |
| `policy_ratify` | Ratify a proposal; records ratifier + reasoning. |
| `policy_supersede` | Retire the active policy and install a new version with `supersedes: <old>@<v>`. |
| `policy_list` | List policies filtered by path prefix and status (`active` / `proposed` / `all`). |
| `policy_show` | Read a policy; `version` pins a historical read. |
| `policy_history` | Walk the supersedes chain for a path (oldest-first). |
| `policy_evaluate` | Authorization evaluation — `(situation, action, agent_id) → Decision`. |
| `policy_evaluate_change` | Change-proposal evaluation — `ChangeProposal → Decision` (with `FallbackAction` on `RequireApproval`). |
| `policy_check_tokens` | Pre-flight: given change tokens, list every active policy whose `triggers` match. |

### Fail-safe translation

`Decision::NoPolicyMatch` is translated at the MCP layer before the
response is returned. The default is `deny` (safe); configure via
`AgentStateGraphServer::with_fail_safe("allow")` when you want a
permissive default. The original `no_policy_match` kind always appears
in the response alongside the translated decision so callers can tell
"authorized by an explicit allow" from "nothing matched, fail-safe
applied."

### `commit_spec` is gated

The existing `commit_spec` tool now builds a `ChangeProposal` from the
speculation's diff and consults `PolicyStore::evaluate_change` before
promoting:

- `Allow` or `NoPolicyMatch` → existing promotion path runs.
- `Deny` or `RequireApproval` → speculation is left untouched and the
  `Decision` JSON is returned so the caller can apply the fallback
  branch (`LowestRiskAlternative`, `PickAlternative`,
  `KeepCurrentState`, `Block`, or `DelegateTo`).

Inferred tokens on the implicit proposal:

- `destructive` — any `RemoveKey` / `RemoveElement` / `RemoveFromSet`
- `schema-change` — any path under `/_meta/schema_version`
- `ref-rewrite` — any `ChangeType` op
- `large` — more than 50 diff ops (see `LARGE_CHANGE_THRESHOLD`)
- `reindex` — any path under `/index/` or a `"reindexed": true` marker
- `migration` — any path under `/_meta/migrations/`

Callers can attach additional proposal metadata via the new
`attached_fields` / `alternatives` parameters on `commit_spec` to
satisfy a policy's `required_fields`.
