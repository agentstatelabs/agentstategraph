---
title: Policy
description: Authorization and cost-of-change gating for agent-driven state, enforced at commit time.
---

Policy is AgentStateGraph's governance primitive. It answers two questions before a change is allowed to land:

1. **Is this action authorized?** — given a situation, an action, and the agent requesting it.
2. **Is this change too expensive or too risky to apply unsupervised?** — gating by the *shape* of a change (destructive, schema-altering, large) independent of whether the action itself is authorized.

Every policy is a versioned, blameable commit in the graph. Policy is **soft enforcement**: a `Deny` is a respected, audited boundary — pair it with hard enforcement (Cedar/OPA/IAM at the perimeter) when you need a runtime kill-switch. The point is that an agent fleet can be governed by data the same way a codebase is governed by tests.

## The lifecycle

Policies move through an explicit, auditable lifecycle. Each step is an ordinary commit, so the whole history shows up in `log` and `blame`.

| Step | What it does |
|------|--------------|
| **propose** | Writes an unratified policy (version 1, no `ratified_by`). The evaluator ignores unratified policies. |
| **ratify** | Sets `ratified_by` / `ratified_at` (plus optional reasoning). The policy becomes live once it is ratified, its `active_from` has passed, and it has not expired. |
| **supersede** | Replaces an active policy with an incremented version; the prior `path@version` is recorded in `supersedes`. Walk the chain with `policy_history`. |
| **sign** | Attaches an Ed25519 signature (independent of propose/ratify). |

## Decisions and fallback

Evaluation produces a `Decision`:

- **`Allow`** — the change may proceed.
- **`Deny`** — the change is blocked.
- **`RequireApproval`** — the change needs sign-off; it carries a **fallback action** describing what to do while approval is pending.
- **`NoPolicyMatch`** — no policy applied. The engine returns this verbatim; the MCP layer applies a fail-safe deny.

Precedence is `deny > require_approval > allow`, so the most restrictive matching policy wins.

Fallback actions make `RequireApproval` actionable instead of just blocking:

- `Block` — hold the change.
- `PickAlternative { action }` — run a specified safer action instead.
- `LowestRiskAlternative` — run the lowest-risk option from the proposal's alternatives.
- `KeepCurrentState` — leave state untouched.
- `DelegateTo { policy_path }` — defer the decision to another policy.

A `ChangeProposal` (action, agent id, intent, preferred option, alternatives, tokens, attached fields) is what you hand to `policy_evaluate_change`.

## Pluggable evaluators

Built-in rules cover the common cases, but you can plug in an external evaluator per policy. The `ExternalEvaluatorRef` selects a backend, and each wraps an `EvaluatorSource` (inline, file path, or commit ref):

- **Cedar** — shells out to `cedar authorize`. `Allow` → Allow, `Deny` → Deny, otherwise `NoPolicyMatch`.
- **Rego** — shells out to `opa eval` (Open Policy Agent).
- **WASM** — runs a WebAssembly module via a wasmtime host, following the [policy evaluator ABI](https://github.com/agentstatelabs/AgentStateGraph) (`asg_alloc` / `asg_free` / `asg_evaluate`).

Register them with `PolicyStore::with_external_evaluators`. Unregistered kinds are simply skipped (treated as not-matching).

## Ed25519 signing

The optional `agentstategraph-policy-sign` crate keeps the signing dependency out of the core. It canonicalizes a policy (sorted keys, no whitespace, the `signature` field excluded), signs those bytes with an `Ed25519Signer`, and stores the result as a `PolicySignature` with a `signer_key_id`. Verification re-canonicalizes, looks up the verifying key by id in a `KeyRegistry`, and runs strict verification.

Configure a store with `with_verifier(...)` and `with_require_signed(true)` to make the evaluator skip any policy that is unsigned or whose signature does not verify — so only cryptographically attested policies can gate changes.

## MCP tools

| Tool | Purpose |
|------|---------|
| `agentstategraph_policy_propose` | Propose a new (unratified) policy |
| `agentstategraph_policy_ratify` | Ratify a proposal so it becomes active |
| `agentstategraph_policy_supersede` | Replace an active policy with a new version |
| `agentstategraph_policy_list` | List policies (path / status / tenant filters) |
| `agentstategraph_policy_show` | Read a policy (active or pinned version) |
| `agentstategraph_policy_history` | Walk a policy's supersedes chain |
| `agentstategraph_policy_evaluate` | Authorization evaluation for a situation |
| `agentstategraph_policy_evaluate_change` | Evaluate a full change proposal; returns a decision + fallback |
| `agentstategraph_policy_check_tokens` | Pre-flight: which active policies match given tokens |
| `agentstategraph_policy_sign` | Sign the active policy at a path |
| `agentstategraph_policy_verify` | Verify the signature on a policy |
| `agentstategraph_policy_evaluate_change_with_taints` | Compose policy + taint into one decision |

See the [MCP Tools reference](/reference/mcp-tools/#policy) for parameters and examples, and [Taint & Quarantine](/guides/taint/) for how the two compose.
