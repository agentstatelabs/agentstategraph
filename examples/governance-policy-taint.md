# Example: Governing a change with policy + taint

This walkthrough shows the two governance layers working together: a **policy**
that gates low-confidence writes to a sensitive path, and a **taint** that
quarantines a path after a security signal. The final call composes both into a
single go/no-go decision.

All snippets are MCP tool calls. See the [Policy](https://agentstategraph.dev/guides/policy/)
and [Taint](https://agentstategraph.dev/guides/taint/) guides for the full model.

## 1. Propose and ratify a policy

Propose a policy that requires high confidence for writes under `/cluster/*`,
then ratify it so it becomes active. (Unratified policies are ignored by the
evaluator.)

```json
// agentstategraph_policy_propose
// `policy` is a full Policy JSON document; `ref` defaults to "main".
{
  "policy": {
    "path": "/cluster",
    "description": "High-confidence gate for cluster writes",
    "rules": [
      { "match": { "path_prefix": "/cluster/" }, "min_confidence": 0.8, "effect": "require_approval" }
    ]
  }
}
```
```json
// agentstategraph_policy_ratify
{ "path": "/cluster", "ratifier": "agent/lead", "reasoning": "Reviewed and approved cluster write gate" }
```

## 2. Evaluate a change before committing

A low-confidence change is gated:

```json
// agentstategraph_policy_evaluate_change
{
  "path": "/cluster/replicas",
  "value": 5,
  "agent_id": "agent/scaler",
  "confidence": 0.65
}
// → Decision: RequireApproval (confidence 0.65 < 0.8) with fallback action
```

A high-confidence change passes:

```json
// agentstategraph_policy_evaluate_change
{
  "path": "/cluster/replicas",
  "value": 5,
  "agent_id": "agent/scaler",
  "confidence": 0.92
}
// → Decision: Allow
```

## 3. Quarantine a path after a security signal

A monitor detects possible credential exposure and quarantines the path. From
now on, only authorized agents can write under it, and the mark is recorded as a
blameable commit.

```json
// agentstategraph_quarantine
{
  "path": "/cluster/credentials",
  "severity": "High",
  "reason": "Possible credential exposure detected by scanner"
}
```

```json
// agentstategraph_check_taint
{ "path": "/cluster/credentials" }
// → { "tainted": true, "effect": "Quarantine", "severity": "High", ... }
```

## 4. Compose policy + taint in one decision

Before committing a sensitive change, ask the combined question. `can_proceed`
is true only when the policy decision is not `deny` **and** every affected path
is writable under its taints.

```json
// agentstategraph_policy_evaluate_change_with_taints
{
  "path": "/cluster/credentials",
  "value": { "token": "rotated-token" },
  "agent_id": "agent/ops",
  "confidence": 0.95
}
// → { "decision": "Allow", "taint_status": "Quarantine", "can_proceed": false }
```

Even though the policy would allow the change, the quarantine blocks it — so the
agent must resolve the quarantine (with proof) before proceeding:

```json
// agentstategraph_unquarantine
{ "path": "/cluster/credentials", "reason": "Credentials rotated and verified clean" }
```

## Key takeaways

- Policy answers "is this authorized / affordable?"; taint answers "has this
  gone wrong?" — they are independent and compose.
- The lifecycle of every policy and every taint is a blameable commit.
- `policy_evaluate_change_with_taints` is the single call to gate a sensitive
  write.
