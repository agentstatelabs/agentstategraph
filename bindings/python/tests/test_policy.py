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
    assert fetched["ratified_by"] is None
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
