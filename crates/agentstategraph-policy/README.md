# agentstategraph-policy

Policy primitive for AgentStateGraph: authorization + procedures +
ratification lifecycle + cost-of-change gating. The crate ships the
`PolicyStore` API and the evaluator; signing, tenant filtering, and
external-evaluator dispatch all compose through this surface.

See `docs/POLICY_GUIDE.md` for the end-user overview and
`spec/POLICY_V1.md` (private) for the design rationale.

## Core types

| Type | Purpose |
|---|---|
| `Policy` | Unit of authorization + procedure (POLICY_V1.md §2.1) |
| `PolicyStore` | Repository-backed store; propose / ratify / supersede / evaluate |
| `Decision` | Result of `evaluate` — `Allow` / `Deny` / `RequireApproval` / `NoPolicyMatch` |
| `ChangeProposal` | Shape-of-change record evaluated via `evaluate_change` (POLICY_V1.md §22.2) |
| `Situation` | Flat `HashMap<String, String>` matched against `Selector` |
| `Selector` | Situation matcher: `Always` / `Eq` / `AllOf` / `AnyOf` |
| `FallbackAction` | What to do while approval is pending (POLICY_V1.md §22.3) |

## Workflow

```rust
use std::sync::Arc;
use agentstategraph::Repository;
use agentstategraph_storage::MemoryStorage;
use agentstategraph_policy::{PolicyStore, Policy, Situation};

let repo = Arc::new(Repository::new(Box::new(MemoryStorage::new())));
repo.init()?;
let store = PolicyStore::new(repo.clone(), "/policies", "ops-bot");

// 1. Propose (anyone can propose).
let policy = Policy { /* ... */ };
store.propose("main", policy)?;

// 2. Ratify (quorum / ratifier enforcement is the consumer's job).
store.ratify("main", "infra/restart", "ops-lead", "LGTM")?;

// 3. Evaluate.
let decision = store.evaluate("main", &Situation::new(), "restart_pod", "agent-1")?;
```

## Advanced features (0.7.5)

### Signing

Opt-in via the sibling `agentstategraph-policy-sign` crate. Policies
carry an optional `Policy.signature`; the evaluator consults a
registered `SignatureVerifier` when `require_signed_policies` is set.
Unsigned policies keep working by default.

```rust
use agentstategraph_policy_sign::{Ed25519Verifier, InMemoryKeyRegistry};

let verifier = Arc::new(Ed25519Verifier::new(keys));
store.set_verifier(Some(verifier));
store.set_require_signed(true);
```

### Multi-tenant

`Policy.tenant_id: Option<String>` — `None` is a global policy; `Some`
restricts to callers that pass a matching `tenant_filter` into the
`_scoped` evaluator variants (`active_scoped`, `evaluate_scoped`,
`evaluate_change_scoped`, `list_scoped`,
`policies_for_situation_scoped`). `None` filter + `Some` tenant_id
still match — globals apply to every tenant.

```rust
let d = store.evaluate_scoped("main", &sit, "action", "agent-1", Some("acme"))?;
```

### External evaluators

`Policy.external_evaluator: Option<ExternalEvaluatorRef>` (`Rego` /
`Cedar` / `Wasm` variants) routes matching policies to a registered
runner. Install runners via
`PolicyStore::with_external_evaluators(Arc<ExternalEvaluatorRegistry>)`.
Runner crates live under `agentstategraph-policy-wasm` / `-rego` /
`-cedar`; see `docs/POLICY-EVALUATOR-ABI.md` for the WASM ABI.

## PolicyStore API — quick reference

```rust
// CRUD
propose(ref, Policy)                       -> String (handle path@version)
ratify(ref, path, ratifier, reasoning)     -> Result<()>
supersede(ref, path, next_policy)          -> Result<String>
get(ref, path, version_opt)                -> Result<Policy>
history(ref, path)                         -> Result<Vec<Policy>>
list(ref, prefix_opt)                      -> Result<Vec<Policy>>
active(ref, prefix_opt)                    -> Result<Vec<Policy>>

// Evaluation
evaluate(ref, situation, action, agent_id) -> Result<Decision>
evaluate_change(ref, proposal)             -> Result<Decision>
check_tokens(ref, tokens)                  -> Result<Vec<Policy>>

// 0.7.5 scoped variants (multi-tenant)
evaluate_scoped(ref, situation, action, agent_id, tenant_filter)
evaluate_change_scoped(ref, proposal, tenant_filter)
active_scoped(ref, prefix_opt, tenant_filter)
list_scoped(ref, prefix_opt, tenant_filter)
policies_for_situation_scoped(ref, situation, tenant_filter)

// 0.7.5 signing hooks
set_verifier(Option<Arc<dyn SignatureVerifier>>)
set_require_signed(bool)
set_signature(ref, path, PolicySignature)  // server-side, after canonicalize

// 0.7.5 external evaluators
with_external_evaluators(Arc<ExternalEvaluatorRegistry>)
```

## Storage layout

Policies live in the state tree under `<prefix>/<path>/<version>`. A
sibling `<prefix>/<path>/_head` points at the current version for
cheap latest-lookup. All writes are committed through the
`Repository` so the full blame / history stack works out of the box.

## Where things live

| Feature | Crate |
|---|---|
| Core types + evaluator | `agentstategraph-policy` |
| Ed25519 signing | `agentstategraph-policy-sign` |
| WASM external evaluator | `agentstategraph-policy-wasm` |
| Rego external evaluator | `agentstategraph-policy-rego` |
| Cedar external evaluator | `agentstategraph-policy-cedar` |
| MCP tool surface | `agentstategraph-mcp` |
| C ABI (for non-Rust consumers) | `agentstategraph-ffi` |

## Soft enforcement

ASG cannot physically stop a misbehaving agent; a `Deny` is a
machine-readable boundary, not a syscall interceptor. Pair with
OPA / Cedar / cloud IAM for hard enforcement at the infrastructure
layer — see POLICY_V1.md §11 for the full soft-model discussion.

## License

BSL-1.1.
