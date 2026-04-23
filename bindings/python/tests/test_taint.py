"""Taint / quarantine / watch Python binding tests (0.7.75-beta.1 §9a).

Exercises the AgentStateGraph PyO3 pass-through for the taint substrate:
taint / untaint / quarantine / unquarantine / watch / unwatch /
list_taints / check_taint, plus the pre-commit hook semantics
(Block rejects, Review requires high confidence, Quarantine gates
by agent_id, Watch auto-escalates when numeric thresholds cross).
"""
import pytest

from agentstategraph_py import AgentStateGraph


@pytest.fixture
def ag():
    asg = AgentStateGraph()
    # Need at least one commit on `main` for session helpers — harmless
    # to seed one here too so downstream tests have a predictable HEAD.
    asg.set_json("/seed", {"v": 0}, description="seed", category="Checkpoint")
    return asg


def _names(taints):
    return sorted(t["name"] for t in taints)


def test_taint_roundtrip(ag):
    """Warn-effect taint lists, survives round-trip, resolves cleanly."""
    tid = ag.taint("main", "/area/alpha", {
        "name": "review-pending",
        "effect": "warn",
        "reason": "needs human review",
        "severity": "medium",
        "agent_id": "pytest",
    })
    assert isinstance(tid, str) and len(tid) > 0

    active = ag.list_taints(path="/area/alpha")
    assert _names(active) == ["review-pending"]
    assert active[0]["kind"] == "taint"
    assert active[0]["effect"] == "warn"
    # `resolved_at` uses skip_serializing_if on the Rust side, so
    # unresolved taints omit the key.
    assert active[0].get("resolved_at") is None

    ag.untaint("main", "/area/alpha", "review-pending", {
        "reason": "human reviewed",
        "agent_id": "pytest",
    })
    assert ag.list_taints(path="/area/alpha") == []
    # Resolved records are hidden by default but visible on demand.
    resolved = ag.list_taints(path="/area/alpha", include_resolved=True)
    assert len(resolved) == 1
    assert resolved[0]["resolved_at"] is not None


def test_block_effect_rejects_set(ag):
    """Block-effect taint on a parent path stops set_json underneath."""
    ag.taint("main", "/x", {
        "name": "hard-freeze",
        "effect": "block",
        "reason": "production freeze",
        "agent_id": "pytest",
    })
    with pytest.raises(RuntimeError) as exc:
        ag.set_json("/x/child", {"v": 1}, description="write", category="Checkpoint")
    assert "blocked" in str(exc.value).lower()


def test_review_effect_requires_high_confidence(ag):
    """Review-effect taint rejects low-confidence writes, accepts high."""
    ag.taint("main", "/r", {
        "name": "review-gate",
        "effect": "review",
        "reason": "needs sign-off",
        "agent_id": "pytest",
    })
    with pytest.raises(RuntimeError) as exc:
        ag.set_json("/r/item", {"v": 1},
                    description="low-conf write",
                    category="Checkpoint",
                    confidence=0.5)
    msg = str(exc.value).lower()
    assert "confidence" in msg or "insufficient" in msg

    # High-confidence write satisfies the 0.9 floor.
    cid = ag.set_json("/r/item", {"v": 2},
                      description="high-conf write",
                      category="Checkpoint",
                      confidence=0.95)
    assert isinstance(cid, str) and len(cid) > 0


def test_quarantine_gates_by_agent(ag):
    """Quarantine on a path blocks unauthorized agents; allows the allowlist."""
    ag.quarantine("main", "/q", {
        "name": "sec-hold",
        "reason": "security review",
        "severity": "high",
        "authorized_agents": ["agent/security"],
        "agent_id": "pytest",
    })
    # Unauthorized agent can't write underneath.
    with pytest.raises(RuntimeError) as exc:
        ag.set_json("/q/item", {"v": 1},
                    description="untrusted write",
                    category="Checkpoint",
                    agent="agent/randos")
    msg = str(exc.value).lower()
    assert "authorized" in msg or "quarantin" in msg or "not authorized" in msg

    # Authorized agent passes.
    cid = ag.set_json("/q/item", {"v": 2},
                      description="trusted write",
                      category="Checkpoint",
                      agent="agent/security")
    assert isinstance(cid, str)

    quarantines = ag.list_taints(path="/q", kind="quarantine")
    assert _names(quarantines) == ["sec-hold"]


def test_watch_auto_escalation(ag):
    """Watch threshold crossed by a set_json produces an auto-taint."""
    ag.watch("main", "/metrics/cpu", {
        "name": "cpu-hot",
        "reason": "auto-escalate when cpu > 80",
        "metric": "cpu",
        "threshold": 80.0,
        "direction": "above",
        "severity": "medium",
        "agent_id": "pytest",
    })
    # Under threshold — no auto-taint.
    ag.set_json("/metrics/cpu", {"cpu": 50.0},
                description="below threshold",
                category="Checkpoint")
    auto_before = [
        t for t in ag.list_taints(path="/metrics/cpu", kind="taint")
        if t["name"].startswith("watch-threshold-exceeded-")
    ]
    assert auto_before == []

    # Above threshold — one auto-taint appears.
    ag.set_json("/metrics/cpu", {"cpu": 95.0},
                description="above threshold",
                category="Checkpoint")
    auto_after = [
        t for t in ag.list_taints(path="/metrics/cpu", kind="taint")
        if t["name"].startswith("watch-threshold-exceeded-")
    ]
    assert len(auto_after) == 1
    assert auto_after[0]["name"] == "watch-threshold-exceeded-cpu-hot"
    assert auto_after[0]["effect"] == "warn"


def test_check_taint_aggregates(ag):
    """check_taint surfaces tainted + quarantined + can_write in one call."""
    ag.taint("main", "/agg", {
        "name": "warn-only",
        "effect": "warn",
        "reason": "advisory",
        "agent_id": "pytest",
    })
    ag.quarantine("main", "/agg", {
        "name": "hold",
        "reason": "security",
        "severity": "high",
        "authorized_agents": ["agent/security"],
        "agent_id": "pytest",
    })
    check = ag.check_taint("/agg", agent_id="agent/randos", confidence=1.0)
    assert check["tainted"] is True
    assert check["quarantined"] is True
    assert check["can_write"] is False
    # Authorized agent sees can_write True.
    check_ok = ag.check_taint("/agg", agent_id="agent/security", confidence=1.0)
    assert check_ok["quarantined"] is True
    assert check_ok["can_write"] is True
