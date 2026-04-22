# Policy Guide

_User-facing guide to the `agentstategraph-policy` crate and the 9 policy
MCP tools. Shipped in 0.6.0-beta.1. For the canonical design rationale
see `/strategy/POLICY_V1.md` (private to agentstatelabs)._

## What Policy is for

Policy is the fourth ASG primitive, alongside **memory**, **tasks**, and
**migrate**. It answers the question that shows up the moment an agent
has real consequences:

> **What am I allowed to do, and what should I do while I wait for someone
> to tell me?**

Concretely, policy gives you:

- **Authorization** — machine-evaluable allow / deny / require-approval
  on (situation, action, agent) triples.
- **Procedures** — declarative runbook steps that can be materialized
  as a plan.
- **Lifecycle** — proposals that humans or agents must ratify before they
  become live; supersede chain for versioning.
- **Cost-of-change gating** — policies can trigger on the *shape* of a
  change (destructive, schema-change, reindex…) independent of what the
  action is authorized to do.
- **The fallback pattern** — when a policy requires approval, it also
  tells the agent what safe alternative to run *right now* so operations
  keep moving while the approval is pending.

The whole thing is versioned, blameable, and recorded as ASG commits —
the same substrate as everything else.

## What Policy is NOT

- **Not a hard runtime enforcer.** ASG cannot physically stop a misbehaving
  agent. A `Deny` decision is a machine-readable boundary that the agent
  is expected to respect; ignoring it leaves an obvious audit trail.
  Pair policy with OPA / Cedar / cloud IAM for hard enforcement at the
  infrastructure layer.
- **Not a Rego clone.** The selector grammar is deliberately small
  (key-value comparisons + boolean combinators). Complex rules reference
  external policy engines; don't try to reinvent them inside ASG.
- **Not signed.** v1 relies on the same attribution model as the rest of
  ASG — the graph owner controls who can write, and every policy write
  carries an agent_id.

## Anatomy of a policy

```jsonc
{
  "path": "/policies/infra/k8s/pod-failing",
  "version": 1,

  // Human-readable + machine-evaluable trigger
  "situation": "A pod in the prod namespace has entered CrashLoopBackOff for more than 5 minutes",
  "situation_selector": {
    "kind": "all",
    "parts": [
      { "kind": "eq", "key": "namespace", "value": "prod" },
      { "kind": "eq", "key": "state", "value": "CrashLoopBackOff" },
      { "kind": "gt", "key": "duration_seconds", "value": 300 }
    ]
  },

  // Authorization rules
  "allow":   [ { "action": "investigate_logs", "preconditions": [] } ],
  "deny":    [ { "action": "delete_namespace", "condition": null } ],
  "require_approval": [
    {
      "action": "rollback_deployment",
      "approvers": ["human", "platform-lead"],
      "timeout": { "secs": 14400, "nanos": 0 },
      "fallback": { "kind": "keep_current_state" }
    }
  ],

  // Optional runbook procedure
  "procedure": [
    { "action": "investigate_logs" },
    { "action": "try_restart", "if_previous_failed": "page_on_call" }
  ],

  // Cost-of-change dimension (§22 in the design)
  "triggers": [],
  "required_fields": [],
  "severity": "medium",

  // Lifecycle
  "proposed_by": "claude-code",
  "proposed_at": "2026-04-21T14:30:00Z",
  "ratified_by": "alice@agentstatelabs",
  "ratified_at": "2026-04-21T15:00:00Z",
  "active_from": "2026-04-21T15:00:00Z",
  "expires_at":  null,
  "supersedes":  null
}
```

Everything serializes as plain JSON. Proposals have `ratified_by: null` —
the evaluator never consults them.

## The decision model

Calling `policy_evaluate(situation, action, agent_id)` or
`policy_evaluate_change(proposal)` returns one of:

```jsonc
{ "kind": "allow",    "matched_policy": "/policies/.../...@3", "preconditions": [...] }
{ "kind": "deny",     "matched_policy": "...@v", "reason": "..." }
{ "kind": "require_approval", "matched_policy": "...@v",
  "approvers": ["human"], "timeout": {...},
  "fallback": { "kind": "lowest_risk_alternative" },
  "approval_task_path": null }
{ "kind": "no_policy_match" }
```

Precedence: **deny** > **require_approval** > **allow**. When multiple
policies match, the strictest wins.

The engine returns `no_policy_match` verbatim. The **MCP layer** applies
the fail-safe translation (default: deny). That means consumers outside
the MCP surface (e.g. a Rust program linking the policy crate directly)
have to decide their own fail-safe. The MCP default is safe.

## The fallback pattern — "what to do while it waits"

`RequireApproval` decisions carry a `fallback: FallbackAction` telling the
agent what to do *immediately* while the approval task sits in the queue:

| Variant | Meaning |
|---|---|
| `block` | Do nothing; record the preferred action and stop. |
| `pick_alternative { action }` | Run this pre-named alternative. |
| `lowest_risk_alternative` | Pick the least-risky from the proposal's `alternatives`. |
| `keep_current_state` | Don't change anything; record the preferred option as a pending upgrade. |
| `delegate_to { policy_path }` | Re-evaluate under a different policy (chain). |

The agent's workflow under `RequireApproval`:

1. Receive the decision.
2. Apply the fallback (safe path runs immediately).
3. Call `plan_add_task` to create the approval task with the deferred
   `ChangeProposal` as the task's `payload` and the originating change
   id as `parent_change`. Set `on_complete: PromoteChange` so the
   upgrade fires when the approval task completes.
4. Complete the originating task with proof — "Applied fallback X;
   preferred option Y pending approval at `<task_path>`."

Every step is a commit with intent. Blame traces the whole tree from the
final applied value back to the policy that authorized the fallback and
the proposal that is pending approval.

## Cost-of-change gating

Most policies describe *what actions are authorized*. Some need to
describe *what changes are expensive to make*, independent of whether
they're authorized. That's what `triggers` + `required_fields` do.

Example: a "high cost change" policy that fires on destructive or
schema-changing operations and requires approval before promoting:

```jsonc
{
  "path": "/policies/change-control/high-cost-change",
  "situation": "Any change requiring downtime, migration, or destructive operations",
  "situation_selector": { "kind": "eq", "key": "kind", "value": "change" },
  "triggers": ["reindex", "migration", "schema-change", "destructive"],
  "required_fields": ["estimated_downtime", "rollback_plan", "approval_authority"],
  "severity": "high",
  "allow": [],
  "deny":  [],
  "require_approval": [{
    "action": "*",
    "approvers": ["human", "platform-lead"],
    "timeout": null,
    "fallback": { "kind": "lowest_risk_alternative" }
  }],
  ...
}
```

### Token inference on `commit_spec`

The MCP server's `commit_spec` tool (which promotes a speculation branch
to main) automatically builds a `ChangeProposal` from the speculation
handle and infers tokens from the diff:

| Token | Inferred when |
|---|---|
| `destructive` | Any delete op appears |
| `schema-change` | `/_meta/schema_version` is touched |
| `ref-rewrite` | A node's shape is rewritten (ChangeType op) |
| `large` | Total changed paths > 50 (configurable) |
| `reindex` | A `"reindexed": true` marker appears anywhere in the diff |
| `migration` | `/_meta/migrations/` is touched |

If any of those match a policy's `triggers`, `commit_spec` consults the
policy before promoting. If the decision is `Deny` or `RequireApproval`,
**promotion does not happen** — the tool returns the `Decision` JSON and
the caller applies the fallback branch.

## The 9 MCP tools

| Tool | When to use |
|---|---|
| `policy_propose` | Draft a new policy (unratified) |
| `policy_ratify` | Mark a proposal active (now consulted by the evaluator) |
| `policy_supersede` | Replace an active policy with a new version |
| `policy_list` | Enumerate active (or proposed) policies, optionally under a prefix |
| `policy_show` | Read a specific policy version |
| `policy_history` | Walk the supersedes chain |
| `policy_evaluate` | Ask "can I do X in situation Y as agent Z?" |
| `policy_evaluate_change` | Ask the same for a structured change proposal with tokens |
| `policy_check_tokens` | Pre-flight: which policies do these tokens hit? |

Full parameter schemas are advertised to MCP clients via the standard
tool-listing mechanism; see `crates/agentstategraph-mcp/README.md`.

## Composing with speculation

The speculation tools (`speculate`, `spec_modify`, `compare`,
`commit_spec`, `discard`) are designed to pair with policy gates:

1. Agent opens multiple speculation branches to try alternatives.
2. Scores them (via `compare`).
3. Calls `commit_spec` on the winner.
4. `commit_spec` consults policy. If a high-cost-change policy fires with
   `RequireApproval`, the promotion is blocked and the decision is
   returned. The agent then:
   - Applies the fallback (perhaps the second-ranked option).
   - Creates an approval task carrying the winner as payload.
   - Discards the other losing handles.

That's the demo moment the thesis points at: the agent knows when to
act (fallback), when to ask (approval task), and what to do while it
waits (the fallback applied, operations running, approval pending,
everything recorded).

## Composing with tasks

`Task` has three optional fields that exist specifically for policy
interplay:

- `payload` — holds the deferred `ChangeProposal` for approval tasks.
- `parent_change` — points back to the change that triggered this task.
- `on_complete: Option<OnCompleteHook>` — `PromoteChange` fires the
  deferred promotion when the approval task completes; `Named { name }`
  delegates to a consumer-registered hook.

The policy crate does not execute hooks; the consumer (MCP server today,
Lens UI tomorrow) dispatches them.

## Soft enforcement — read this before marketing

From `POLICY_V1.md` §11: *"CTXone cannot physically stop a misbehaving
agent from doing something a policy denies. The evaluation tells the
agent what's allowed; the agent still has to respect the decision."*

The value is:

1. **Clarity** — the agent always knows what it's authorized to do.
2. **Audit trail** — every decision is recorded.
3. **Deterrent** — ignoring a `Deny` leaves an obvious trace.
4. **Composition** — pair with OPA / Cedar / IAM for hard enforcement at
   the infrastructure layer.

Name this upfront. Overselling "stops rogue AI agents" fails the first
time anyone tries to verify it.

## 0.7.5 — Advanced policy

Three independent features landed in 0.7.5-beta.1. Each composes with
the rest of the substrate and each is opt-in.

### Signing (`agentstategraph-policy-sign`)

Optional sibling crate. Keeps `ed25519-dalek` out of the main policy
crate so consumers who don't sign don't pay the crypto cost.

- `Policy.signature: Option<PolicySignature>` — a tagged union;
  Ed25519 is the only variant shipped. Payload: `{algorithm,
  signer_key_id, signature_hex}`.
- Canonical JSON: sorted keys, no whitespace, UTF-8, excludes the
  `signature` field itself. Sign the bytes, store the hex. Verify by
  re-canonicalizing + checking against a `KeyRegistry`-registered
  public key.
- Server-side: `AgentStateGraphServer::with_signer(...)` +
  `with_policy_verifier(...)`. When `with_require_signed_policies(true)`
  is set and a verifier is installed, policies without a valid
  signature are treated as not-currently-active (skipped). Unsigned
  policies continue to work by default — the verifier is opt-in.
- MCP tools: `policy_sign(ref, path, signer_key_id)` and
  `policy_verify(ref, path)`. FFI externs
  `agentstategraph_policy_sign` + `agentstategraph_policy_verify`.

Key rotation + CRL semantics are scheduled for pre-GA.

### Multi-tenant

Cheap namespace discriminator per ROADMAP D3.

- `Policy.tenant_id: Option<String>` — `None` is a global policy that
  applies to every tenant; `Some(id)` restricts the policy to callers
  that pass a matching `tenant_filter` at evaluation time.
- `Session.scope_tenant: Option<String>` carries the session-level
  default.
- `PolicyStore::{active, evaluate, evaluate_change, list,
  policies_for_situation}_scoped` take the `tenant_filter`. Zero-arg
  methods remain as back-compat wrappers that default
  `tenant_filter = None`.
- Semantics: `tenant_filter = None` returns everything (globals +
  every tenant's policies). `tenant_filter = Some("acme")` returns
  policies whose `tenant_id == Some("acme")` OR `tenant_id == None`.
  Globals apply to every tenant.

MCP tools that take a situation / action / proposal accept an
optional `tenant_filter` parameter.

### External evaluators

Escape-hatch for Rego / Cedar / WASM policy engines. See
`docs/POLICY-EVALUATOR-ABI.md` for the WASM ABI contract.

- `Policy.external_evaluator: Option<ExternalEvaluatorRef>` — a
  tagged union of `Rego { source }`, `Cedar { source }`, `Wasm
  { source }`. `EvaluatorSource` is itself a tagged union of
  `Inline { body }`, `FilePath { path }`, `CommitRef { path }`.
- Runner crates (each opt-in):
  - `agentstategraph-policy-wasm` — wasmtime host, documented ABI
  - `agentstategraph-policy-rego` — subprocess `opa eval`
  - `agentstategraph-policy-cedar` — subprocess `cedar` CLI (stub in
    0.7.5-beta.1; concrete wiring TBD)
- Server builders: `with_external_evaluator(Arc<dyn
  ExternalEvaluator>)`, plus feature-gated convenience helpers
  `with_wasm_evaluator()`, `with_rego_evaluator()`,
  `with_cedar_evaluator()`.
- Dispatch: `external_evaluator == None` → local evaluator. External
  ref whose kind is registered → runner. Kind NOT registered →
  policy falls through the local path unchanged (the external ref is
  inert metadata until a matching runner is installed).
- FFI: `agentstategraph_policy_set_external_evaluator` is a stub in
  0.7.5-beta.1 (returns a documented error envelope); bindings
  register runners through the MCP server's in-process builders.

## Composing with taints

0.7.75 added the [taint / quarantine / watch substrate](TAINT_GUIDE.md)
— dynamic runtime markers that complement policies. Policies answer
"what is allowed"; taints answer "what has gone wrong". Use
`agentstategraph_policy_evaluate_change_with_taints` to get both
verdicts in one response — `can_proceed` is the conjunction of
`decision.kind != deny` and every affected path's
`check_taint.can_write`.

## Related docs

- `spec/AGENTSTATEGRAPH-RFC.md` — the primitive substrate
- `spec/POLICY-IMPLEMENTATION-PLAN.md` — the 0.6.0-beta.1 execution plan
- `spec/SECURITY-THREAT-MODEL.md` — threat surfaces across the stack
- `spec/0.7.5-PLAN.md` — advanced policy implementation plan
- `spec/0.7.75-PLAN.md` — taint / quarantine / watch implementation plan
- `docs/POLICY-EVALUATOR-ABI.md` — WASM external-evaluator ABI
- `docs/TAINT_GUIDE.md` — taint substrate user guide
- `crates/agentstategraph-mcp/README.md` — MCP tool surface
- `crates/agentstategraph-policy/README.md` — crate API reference
- `crates/agentstategraph-taint/README.md` — taint crate API reference
