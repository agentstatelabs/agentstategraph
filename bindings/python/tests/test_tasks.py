"""TaskStore Python binding tests."""
import pytest

from agentstategraph_py import AgentStateGraph, TaskStore


@pytest.fixture
def store():
    asg = AgentStateGraph()
    return asg, TaskStore(asg, "/plans", "pytest")


def test_create_plan_and_add_tasks(store):
    _, ts = store
    plan = ts.create_plan("main", "website-v2", "Brand pivot")
    assert plan["name"] == "website-v2"
    assert plan["status"] == "active"
    assert plan["description"] == "Brand pivot"

    t1 = ts.add_task("main", "website-v2", "Rewrite hero", "high")
    assert t1["id"] == "t-001"
    assert t1["status"] == "pending"
    assert t1["priority"] == "high"

    t2 = ts.add_task("main", "website-v2", "Ship CSS", "medium")
    assert t2["id"] == "t-002"

    tasks = ts.list_tasks("main", "website-v2")
    assert [t["id"] for t in tasks] == ["t-001", "t-002"]
    assert ts.task_ids("main", "website-v2") == ["t-001", "t-002"]


def test_start_complete_flow(store):
    _, ts = store
    ts.create_plan("main", "p", None)
    t = ts.add_task("main", "p", "thing", "high")
    started = ts.start_task("main", "p", t["id"])
    assert started["status"] == "in_progress"
    done = ts.complete_task("main", "p", t["id"], "commit", "abc123", "verified")
    assert done["status"] == "done"
    assert done["proof"]["kind"] == "commit"
    assert done["proof"]["value"] == "abc123"
    assert done["proof"]["note"] == "verified"

    # plan auto-promoted to completed
    plan = ts.get_plan("main", "p")
    assert plan["status"] == "completed"


def test_blocker_nonexistent_rejected(store):
    _, ts = store
    ts.create_plan("main", "p", None)
    with pytest.raises(RuntimeError):
        ts.add_task("main", "p", "x", "medium", None, ["t-999"])


def test_next_task_picks_highest_priority_unblocked(store):
    _, ts = store
    ts.create_plan("main", "p", None)
    ts.add_task("main", "p", "low", "low")
    high = ts.add_task("main", "p", "high", "high")
    ts.add_task("main", "p", "critical_blocked", "critical", None, [high["id"]])

    # Highest unblocked is `high` (critical one is blocked by high).
    nxt = ts.next_task("main", "p")
    assert nxt["id"] == high["id"]


def test_assign_unassign_roundtrip(store):
    _, ts = store
    ts.create_plan("main", "p", None)
    t = ts.add_task("main", "p", "x", "medium")
    assigned = ts.assign_task("main", "p", t["id"], "codex")
    assert assigned["assigned_to"] == "codex"
    unassigned = ts.unassign_task("main", "p", t["id"])
    assert unassigned["assigned_to"] is None


def test_list_plans_by_status(store):
    _, ts = store
    ts.create_plan("main", "a", None)
    ts.create_plan("main", "b", None)
    ts.archive_plan("main", "b")

    active = ts.list_plans_by_status("main", "active")
    archived = ts.list_plans_by_status("main", "archived")
    assert [p["name"] for p in active] == ["a"]
    assert [p["name"] for p in archived] == ["b"]


def test_next_task_for_assignment_filter(store):
    _, ts = store
    ts.create_plan("main", "p", None)
    mine = ts.add_task("main", "p", "mine", "medium", None, None, "alice")
    ts.add_task("main", "p", "theirs", "high", None, None, "bob")
    unassigned = ts.add_task("main", "p", "free", "medium")

    only_alice = ts.next_task_for("main", "p", "alice", False)
    assert only_alice["id"] == mine["id"]

    alice_or_free = ts.next_task_for("main", "p", "alice", True)
    # `mine` and `free` are both medium; ascending id wins → mine.
    assert alice_or_free["id"] in (mine["id"], unassigned["id"])


def test_verify_plan_with_kinds(store):
    _, ts = store
    ts.create_plan("main", "p", None)
    t = ts.add_task("main", "p", "x", "medium")
    ts.start_task("main", "p", t["id"])
    ts.complete_task("main", "p", t["id"], "commit", "deadbeef")

    report = ts.verify_plan_with_kinds("main", "p", {"commit": True})
    assert report["verified_count"] == 1
    assert report["all_strongly_verified"] is True
