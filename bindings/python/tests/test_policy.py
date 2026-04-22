"""PolicyStore Python binding tests.

Covers POLICY_V1.md §§5, 22.2, 22.3, 22.4 end-to-end through the PyO3
wrapper: propose / ratify / supersede / evaluate / evaluate_change /
check_tokens, plus active_from scheduled activation (landed in §1 of
the 0.7.0 plan), plus the Session round-trip audit and the Task
payload / parent_change / on_complete extension fields.
"""
from datetime import datetime, timedelta, timezone

import pytest

from agentstategraph_py import AgentStateGraph, PolicyStore, TaskStore


def _utc(dt: datetime) -> str:
    return dt.astimezone(timezone.utc).isoformat().replace("+00:00", "Z")


def _policy(path: str, *, allow=None, deny=None, require_approval=None,
            triggers=None, required_fields=None, severity="low",
            active_from=None, situation_selector=None):
    """Build a plain-dict Policy suitable for PolicyStore.propose/supersede."""
    now = datetime.now(timezone.utc)
    return {
        "path": path,
        "version": 1,
        "situation": f"situation for {path}",
        "situation_selector": situation_selector or {"kind": "always"},
        "allow": allow or [],
        "deny": deny or [],
        "require_approval": require_approval or [],
        "triggers": triggers or [],
        "required_fields": required_fields or [],
        "severity": severity,
        "proposed_by": "pytest",
        "proposed_at": _utc(now),
        "active_from": _utc(active_from or now),
    }


@pytest.fixture
def store():
    asg = AgentStateGraph()
    ps = PolicyStore(asg, "/policies", "pytest")
    return asg, ps


def test_propose_creates_unratified_policy(store):
    _, ps = store
    handle = ps.propose("main", _policy("infra/k8s/pod-failing"))
    assert handle == "infra/k8s/pod-failing@1"
    fetched = ps.get("main", "infra/k8s/pod-failing", None)
    assert fetched["version"] == 1
    # `ratified_by` uses `skip_serializing_if = "Option::is_none"`, so
    # unratified policies omit the key entirely.
    assert fetched.get("ratified_by") is None
    assert fetched["proposed_by"] == "pytest"


def test_ratify_promotes_policy(store):
    _, ps = store
    ps.propose("main", _policy("infra/restart", allow=[{"action": "restart_pod"}]))
    ps.ratify("main", "infra/restart", "ops-lead", "approved after review")
    p = ps.get("main", "infra/restart", None)
    assert p["ratified_by"] == "ops-lead"
    assert p["ratification_reasoning"] == "approved after review"
    assert p["ratified_at"] is not None


def test_supersede_chain_and_history(store):
    _, ps = store
    ps.propose("main", _policy("infra/scale", allow=[{"action": "scale_up"}]))
    ps.ratify("main", "infra/scale", "ops", "v1")
    new_v = _policy("infra/scale", allow=[{"action": "scale_up"}, {"action": "scale_down"}])
    new_v["ratified_by"] = "ops"
    new_v["ratified_at"] = _utc(datetime.now(timezone.utc))
    handle = ps.supersede("main", "infra/scale", new_v)
    assert handle == "infra/scale@2"
    history = ps.history("main", "infra/scale")
    assert [p["version"] for p in history] == [1, 2]
    assert history[-1]["supersedes"] == "infra/scale@1"


def test_evaluate_allow(store):
    _, ps = store
    ps.propose(
        "main",
        _policy(
            "infra/restart",
            allow=[{"action": "restart_pod"}],
            situation_selector={"kind": "eq", "key": "namespace", "value": "prod"},
        ),
    )
    ps.ratify("main", "infra/restart", "ops", "ok")
    d = ps.evaluate("main", {"namespace": "prod"}, "restart_pod", "agent-1")
    assert d["kind"] == "allow"
    assert d["matched_policy"] == "infra/restart@1"


def test_evaluate_deny(store):
    _, ps = store
    ps.propose(
        "main",
        _policy(
            "infra/no-delete",
            deny=[{"action": "delete_node", "condition": "always"}],
        ),
    )
    ps.ratify("main", "infra/no-delete", "ops", "ok")
    d = ps.evaluate("main", {}, "delete_node", "agent-1")
    assert d["kind"] == "deny"


def test_evaluate_require_approval(store):
    _, ps = store
    ps.propose(
        "main",
        _policy(
            "infra/risky",
            require_approval=[
                {
                    "action": "truncate_index",
                    "approvers": ["human"],
                    "fallback": {"kind": "block"},
                }
            ],
        ),
    )
    ps.ratify("main", "infra/risky", "ops", "ok")
    d = ps.evaluate("main", {}, "truncate_index", "agent-1")
    assert d["kind"] == "require_approval"
    assert d["approvers"] == ["human"]
    assert d["fallback"]["kind"] == "block"


def test_evaluate_no_match(store):
    _, ps = store
    d = ps.evaluate("main", {}, "anything", "agent-1")
    assert d["kind"] == "no_policy_match"


def test_evaluate_change_with_triggers_and_fallback(store):
    _, ps = store
    ps.propose(
        "main",
        _policy(
            "infra/high-cost",
            triggers=["reindex", "downtime"],
            required_fields=["estimated_downtime"],
            require_approval=[
                {
                    "action": "promote",
                    "approvers": ["human"],
                    "fallback": {"kind": "lowest_risk_alternative"},
                }
            ],
            severity="high",
        ),
    )
    ps.ratify("main", "infra/high-cost", "ops", "big changes need approval")
    # Proposal whose tokens intersect → policy consulted.
    proposal = {
        "action": "promote",
        "agent_id": "agent-1",
        "intent": "merge option C",
        "preferred_option": "spec-7",
        "alternatives": ["spec-1", "spec-3"],
        "tokens": ["reindex"],
        "attached_fields": {"estimated_downtime": "5m"},
    }
    d = ps.evaluate_change("main", proposal)
    assert d["kind"] == "require_approval"
    assert d["fallback"]["kind"] == "lowest_risk_alternative"


def test_evaluate_change_missing_required_fields(store):
    _, ps = store
    ps.propose(
        "main",
        _policy(
            "infra/needs-downtime",
            triggers=["reindex"],
            required_fields=["estimated_downtime"],
            require_approval=[
                {
                    "action": "promote",
                    "approvers": ["human"],
                    "fallback": {"kind": "block"},
                }
            ],
        ),
    )
    ps.ratify("main", "infra/needs-downtime", "ops", "ok")
    # attached_fields missing estimated_downtime → short-circuits to
    # RequireApproval regardless of the evaluate result.
    proposal = {
        "action": "promote",
        "agent_id": "agent-1",
        "intent": "",
        "preferred_option": "x",
        "tokens": ["reindex"],
        "attached_fields": {},
    }
    d = ps.evaluate_change("main", proposal)
    assert d["kind"] == "require_approval"


def test_evaluate_ignores_not_yet_active_policy(store):
    """§1 of the plan: active_from in the future → skipped."""
    _, ps = store
    future = datetime.now(timezone.utc) + timedelta(hours=1)
    pol = _policy(
        "infra/future",
        allow=[{"action": "do_it"}],
        active_from=future,
    )
    ps.propose("main", pol)
    ps.ratify("main", "infra/future", "ops", "scheduled")
    # Ratified but not yet active → no match.
    d = ps.evaluate("main", {}, "do_it", "agent-1")
    assert d["kind"] == "no_policy_match"
    # active() filter agrees.
    actives = ps.active("main", None)
    assert all(p["path"] != "infra/future" for p in actives)


def test_check_tokens_filters_by_trigger_intersection(store):
    _, ps = store
    ps.propose(
        "main",
        _policy("infra/with-reindex", triggers=["reindex"]),
    )
    ps.ratify("main", "infra/with-reindex", "ops", "ok")
    ps.propose(
        "main",
        _policy("infra/with-network", triggers=["network"]),
    )
    ps.ratify("main", "infra/with-network", "ops", "ok")
    matched = ps.check_tokens("main", ["reindex"])
    paths = sorted(p["path"] for p in matched)
    assert paths == ["infra/with-reindex"]
    # Both tokens → both policies.
    matched_all = ps.check_tokens("main", ["reindex", "network"])
    assert sorted(p["path"] for p in matched_all) == [
        "infra/with-network",
        "infra/with-reindex",
    ]


def test_list_and_active_filters(store):
    _, ps = store
    ps.propose("main", _policy("infra/a"))
    ps.propose("main", _policy("infra/b"))
    ps.ratify("main", "infra/b", "ops", "ok")
    listed = ps.list("main", None)
    assert sorted(p["path"] for p in listed) == ["infra/a", "infra/b"]
    actives = ps.active("main", None)
    assert [p["path"] for p in actives] == ["infra/b"]
    # prefix filter
    only_a = ps.list("main", "infra/a")
    assert [p["path"] for p in only_a] == ["infra/a"]


# ---------------------------------------------------------------------------
# Session / SessionStatus round-trip audit
# ---------------------------------------------------------------------------


def test_session_roundtrip_via_agentstategraph():
    asg = AgentStateGraph()
    s = asg.create_session(
        agent_id="agent/planner",
        working_branch="main",
        path_scope="/plans/",
    )
    assert s["agent_id"] == "agent/planner"
    assert s["working_branch"] == "main"
    assert s["status"] == "active"
    assert s["path_scope"] == "/plans/"
    assert s["head"].startswith("sg_")
    assert s["ended_at"] is None

    fetched = asg.get_session(s["id"])
    assert fetched["id"] == s["id"]
    listed = asg.list_sessions(None)
    assert any(x["id"] == s["id"] for x in listed)

    asg.end_session(s["id"], "completed")
    ended = asg.get_session(s["id"])
    assert ended["status"] == "completed"
    assert ended["ended_at"] is not None


# ---------------------------------------------------------------------------
# Task extension fields (payload / parent_change / on_complete) round-trip
# ---------------------------------------------------------------------------


def test_task_extension_fields_roundtrip():
    asg = AgentStateGraph()
    ts = TaskStore(asg, "/plans", "pytest")
    ts.create_plan("main", "p", None)
    t = ts.add_task(
        "main",
        "p",
        "approve high-cost change",
        "high",
        payload={"proposal": {"preferred_option": "spec-7"}},
        parent_change="spec-7@42",
        on_complete={"kind": "promote_change"},
    )
    assert t["parent_change"] == "spec-7@42"
    assert t["payload"] == {"proposal": {"preferred_option": "spec-7"}}
    assert t["on_complete"] == {"kind": "promote_change"}

    fetched = ts.get_task("main", "p", t["id"])
    assert fetched["payload"] == {"proposal": {"preferred_option": "spec-7"}}
    assert fetched["parent_change"] == "spec-7@42"
    assert fetched["on_complete"] == {"kind": "promote_change"}

    # Named hook variant round-trips too.
    t2 = ts.add_task(
        "main",
        "p",
        "custom hook",
        "low",
        on_complete={"kind": "named", "name": "notify-slack"},
    )
    assert t2["on_complete"] == {"kind": "named", "name": "notify-slack"}

    # No extension fields → all three serialize as None.
    t3 = ts.add_task("main", "p", "plain", "low")
    assert t3["payload"] is None
    assert t3["parent_change"] is None
    assert t3["on_complete"] is None


# ---------------------------------------------------------------------------
# 0.7.5 §5a: signing + multi-tenant + external-evaluator field round-trips
# ---------------------------------------------------------------------------


def test_policy_signature_field_round_trips(store):
    """§2a/b: Policy.signature is a tagged union keyed by 'algorithm'."""
    _, ps = store
    pol = _policy("infra/signed", allow=[{"action": "touch"}])
    pol["signature"] = {
        "algorithm": "ed25519",
        "signer_key_id": "ops-root-2026",
        "signature_hex": "aa" * 64,
    }
    ps.propose("main", pol)
    fetched = ps.get("main", "infra/signed", None)
    assert fetched["signature"] == {
        "algorithm": "ed25519",
        "signer_key_id": "ops-root-2026",
        "signature_hex": "aa" * 64,
    }


def test_policy_tenant_id_field_round_trips(store):
    """§3a: Policy.tenant_id is an Option<String>."""
    _, ps = store
    pol = _policy("infra/scoped", allow=[{"action": "touch"}])
    pol["tenant_id"] = "tenant-acme"
    ps.propose("main", pol)
    fetched = ps.get("main", "infra/scoped", None)
    assert fetched["tenant_id"] == "tenant-acme"

    # None (global) policy: omit the field.
    pol2 = _policy("infra/global", allow=[{"action": "touch"}])
    ps.propose("main", pol2)
    fetched2 = ps.get("main", "infra/global", None)
    # serde skip_serializing_if=Option::is_none means absent-or-None.
    assert fetched2.get("tenant_id") is None


def test_policy_external_evaluator_field_round_trips(store):
    """§4a: Policy.external_evaluator is a tagged union with three kinds
    (rego/cedar/wasm) × three source kinds (inline/file_path/commit_ref)."""
    _, ps = store
    matrix = [
        ("rego", "a", {"kind": "inline", "body": "package asg\nallow { true }"}),
        ("cedar", "b", {"kind": "file_path", "path": "/etc/asg/policy.cedar"}),
        ("wasm", "c", {"kind": "commit_ref", "path": "/evaluators/x.wasm"}),
        ("rego", "d", {"kind": "file_path", "path": "/etc/asg/policy.rego"}),
        ("cedar", "e", {"kind": "inline", "body": "permit(principal, action, resource);"}),
        ("wasm", "f", {"kind": "inline", "body": "AGFzbQEAAAA="}),
        ("rego", "g", {"kind": "commit_ref", "path": "/evaluators/rbac.rego"}),
        ("cedar", "h", {"kind": "commit_ref", "path": "/evaluators/corp.cedar"}),
        ("wasm", "i", {"kind": "file_path", "path": "/etc/asg/runner.wasm"}),
    ]
    for kind, suffix, source in matrix:
        pol = _policy(f"infra/ext-{suffix}")
        pol["external_evaluator"] = {"kind": kind, "source": source}
        ps.propose("main", pol)
        fetched = ps.get("main", f"infra/ext-{suffix}", None)
        assert fetched["external_evaluator"] == {"kind": kind, "source": source}


def test_evaluate_with_tenant_filter_scoped_policy(store):
    """§3b: tenant_filter=Some(tid) restricts evaluate() to policies
    whose tenant_id matches or is None."""
    _, ps = store
    acme = _policy(
        "infra/acme-only",
        allow=[{"action": "deploy"}],
        situation_selector={"kind": "always"},
    )
    acme["tenant_id"] = "tenant-acme"
    ps.propose("main", acme)
    ps.ratify("main", "infra/acme-only", "ops", "ok")

    other = _policy(
        "infra/other-only",
        allow=[{"action": "deploy"}],
        situation_selector={"kind": "always"},
    )
    other["tenant_id"] = "tenant-other"
    ps.propose("main", other)
    ps.ratify("main", "infra/other-only", "ops", "ok")

    # acme tenant sees only the acme policy.
    d = ps.evaluate("main", {}, "deploy", "agent-1", tenant_filter="tenant-acme")
    assert d["kind"] == "allow"
    assert d["matched_policy"] == "infra/acme-only@1"

    # Non-matching tenant → no match (other's policy is filtered out,
    # and acme's policy is filtered out from the unrelated tenant).
    d2 = ps.evaluate("main", {}, "deploy", "agent-1", tenant_filter="tenant-unknown")
    assert d2["kind"] == "no_policy_match"

    # active() with tenant_filter also agrees.
    acme_actives = ps.active("main", None, "tenant-acme")
    assert [p["path"] for p in acme_actives] == ["infra/acme-only"]


def test_evaluate_with_tenant_filter_global_fallback(store):
    """§3b: tenant_id=None policies apply under every tenant_filter."""
    _, ps = store
    globally = _policy(
        "infra/global-allow",
        allow=[{"action": "noop"}],
        situation_selector={"kind": "always"},
    )
    ps.propose("main", globally)
    ps.ratify("main", "infra/global-allow", "ops", "ok")

    # Any tenant filter still sees the global policy.
    for tf in ("tenant-a", "tenant-b", None):
        d = ps.evaluate("main", {}, "noop", "agent-1", tenant_filter=tf)
        assert d["kind"] == "allow", f"tenant_filter={tf!r}"
        assert d["matched_policy"] == "infra/global-allow@1"


def test_evaluate_change_with_tenant_filter(store):
    """§3b: evaluate_change also accepts a tenant_filter."""
    _, ps = store
    pol = _policy(
        "infra/tenant-change",
        triggers=["reindex"],
        require_approval=[
            {
                "action": "promote",
                "approvers": ["human"],
                "fallback": {"kind": "block"},
            }
        ],
    )
    pol["tenant_id"] = "tenant-a"
    ps.propose("main", pol)
    ps.ratify("main", "infra/tenant-change", "ops", "ok")

    proposal = {
        "action": "promote",
        "agent_id": "agent-1",
        "intent": "",
        "preferred_option": "x",
        "tokens": ["reindex"],
        "attached_fields": {},
    }
    # Matching tenant → policy consulted.
    d = ps.evaluate_change("main", proposal, tenant_filter="tenant-a")
    assert d["kind"] == "require_approval"
    # Different tenant → policy filtered out, no match.
    d2 = ps.evaluate_change("main", proposal, tenant_filter="tenant-b")
    assert d2["kind"] == "no_policy_match"


def test_session_scope_tenant_field_round_trips():
    """§3a: Session.scope_tenant surfaces in the Python dict (as None
    when unset — SessionManager doesn't yet expose a setter)."""
    asg = AgentStateGraph()
    s = asg.create_session(agent_id="agent/a", working_branch="main")
    assert "scope_tenant" in s
    assert s["scope_tenant"] is None

    fetched = asg.get_session(s["id"])
    assert "scope_tenant" in fetched
    assert fetched["scope_tenant"] is None

    listed = asg.list_sessions(None)
    assert all("scope_tenant" in x for x in listed)


def test_policystore_set_external_evaluator_returns_stub_envelope(store):
    """§5a: set_external_evaluator remains a stub returning an
    {"error": ...} envelope — PolicyStore has no post-propose mutator
    for the per-policy external_evaluator field."""
    _, ps = store
    ps.propose("main", _policy("infra/to-sign"))

    ext = ps.set_external_evaluator(
        "main",
        "infra/to-sign",
        {"kind": "rego", "source": {"kind": "inline", "body": "package x"}},
    )
    assert ext.get("error") == "not yet wired"


def test_policystore_sign_requires_signature_hex(store):
    """sign() without signature_hex returns a structured error — the
    binding does not yet ship a local Ed25519 signer."""
    _, ps = store
    ps.propose("main", _policy("infra/to-sign"))

    # No args beyond signer_key_id → error asking for signature_hex.
    missing = ps.sign("main", "infra/to-sign", "key-1")
    assert missing.get("error") == "signature_hex required"

    # private_key_hex is accepted syntactically but currently refused
    # until the binding grows the crypto dep.
    refused = ps.sign(
        "main", "infra/to-sign", "key-1", private_key_hex="ab" * 32
    )
    assert refused.get("error") == "local signing not available"
    assert "hint" in refused


def test_policy_sign_produces_valid_signature(store):
    """propose + sign writes a PolicySignature onto the policy.

    Because the binding can't produce an Ed25519 signature locally
    (no `agentstategraph-policy-sign` dep in bindings/python/Cargo.toml
    yet), we supply a pre-computed hex placeholder. The signature round-
    trips through `set_signature` and re-fetch, which is what this test
    validates.
    """
    _, ps = store
    ps.propose("main", _policy("infra/sign-target"))

    # Placeholder 64-byte signature. A real caller would compute this
    # over canonical JSON via a Python crypto library.
    sig_hex = "aa" * 64
    key_id = "pytest-ephemeral-key"

    resp = ps.sign("main", "infra/sign-target", key_id, signature_hex=sig_hex)
    assert resp.get("ok") is True
    assert resp["handle"] == "infra/sign-target@1"
    assert resp["signer_key_id"] == key_id

    fetched = ps.get("main", "infra/sign-target", None)
    assert fetched["signature"] == {
        "algorithm": "ed25519",
        "signer_key_id": key_id,
        "signature_hex": sig_hex,
    }


def test_policy_verify_round_trip(store):
    """sign then verify returns {"valid": true}.

    SKIPPED: verify() requires a registered `SignatureVerifier` on the
    PolicyStore, which the Python binding does not install. Wiring the
    verifier through PyO3 needs:
      - `agentstategraph-policy-sign` + `ed25519-dalek` + `hex` added to
        `bindings/python/Cargo.toml`
      - a PyO3 `KeyRegistry` wrapper so Python callers can register
        public keys by id
      - `PolicyStore.__new__` accepting an optional verifier kwarg and
        calling `PolicyBackend::with_verifier` at construction
    Until that plumbing lands, verify() returns a structured
    {"valid": false, "reason": "no verifier registered"} envelope — NOT
    the pre-wiring stub.
    """
    _, ps = store
    ps.propose("main", _policy("infra/verify-target"))
    ps.sign(
        "main",
        "infra/verify-target",
        "pytest-key",
        signature_hex="bb" * 64,
    )

    # Sanity: the structured-error shape is what we return today.
    result = ps.verify("main", "infra/verify-target")
    assert result.get("valid") is False
    assert "no verifier registered" in result.get("reason", "")

    pytest.skip(
        "verify() needs agentstategraph-policy-sign dep + PyO3 "
        "KeyRegistry wrapper + PolicyStore.with_verifier plumbing; "
        "see docstring above"
    )
