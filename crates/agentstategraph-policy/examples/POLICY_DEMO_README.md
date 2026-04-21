# policy_demo — runnable walkthrough of POLICY_V1.md §22.7

```
cargo run --example policy_demo -p agentstategraph-policy
```

## What it shows

An autonomous agent evaluates three OpenSearch tuning options via
speculation, scored 3/7/9. The highest-scoring option (C) requires a
reindex, which trips a ratified `high-cost-change` policy with a
`LowestRiskAlternative` fallback. The demo prints:

1. Which policies each proposal matches, via `PolicyStore::evaluate_change`.
2. The full `RequireApproval` decision returned for Option C.
3. The agent's playbook: apply the fallback immediately, create an
   approval task carrying Option C as payload, report proof.
4. The ASG commit log — every policy write is a first-class commit.

## Why

This is the concrete demo artefact for the thesis (POLICY_V1.md §22.1):

> An AI agent that knows when to act, when to ask, and what to do
> while it waits — and all of it recorded, auditable, transparent,
> and sealed for export.

The narrator script in POLICY_V1.md §22.7 builds on these exact
outputs.

## Relationship to the MCP `compare` tool

The demo constructs its `ChangeProposal` objects directly (tokens
attached inline). In a real MCP session the tokens come from the
`compare` tool's response (0.6.75-beta.1 §6 extends `compare` to emit
the same `infer_tokens_from_diff` output that `commit_spec` uses
internally). An agent calling `compare` before `commit_spec` can see
which handles will hit which policies before committing to a
promotion.
