# agentstategraph (Python)

Python bindings for [AgentStateGraph](https://github.com/agentstatelabs/agentstategraph).

## Install

```bash
pip install maturin
maturin develop --release
```

## Versioned state

```python
from agentstategraph_py import AgentStateGraph

asg = AgentStateGraph()                     # in-memory
asg = AgentStateGraph("./state.db")         # SQLite

asg.set("/name", "my-cluster", "init", category="Checkpoint")
print(asg.get("/name"))
```

## TaskStore

`TaskStore` is a plans-and-tasks layer on top of an `AgentStateGraph`.

```python
from agentstategraph_py import AgentStateGraph, TaskStore

asg = AgentStateGraph()
tasks = TaskStore(asg, "/plans", "claude-code")

tasks.create_plan("main", "website-v2", "Brand pivot")
t = tasks.add_task("main", "website-v2", "Rewrite hero", "high")
tasks.start_task("main", "website-v2", t["id"])
tasks.complete_task("main", "website-v2", t["id"], "commit", "abc123")

# Pick the highest-priority unblocked task:
nxt = tasks.next_task("main", "website-v2")

# Filter by assignee:
mine = tasks.next_task_for("main", "website-v2", "claude-code", include_unassigned=True)

# Verify `done` tasks. verify_by_kind maps proof kinds -> bool; true kinds
# are reported as Verified, others as Unverifiable.
report = tasks.verify_plan_with_kinds(
    "main", "website-v2", {"commit": True, "file": True}
)
print(report["summary"])
```

Priority values: `"low"`, `"medium"`, `"high"`, `"critical"`.
Task status: `"pending"`, `"in_progress"`, `"done"`, `"abandoned"`.
Plan status: `"active"`, `"completed"`, `"archived"`.
Proof kind: `"commit"`, `"file"`, `"test"`, `"text"`.

## Schema migrations

```python
asg = AgentStateGraph("./state.db")
result = asg.check_schema()
if result["status"] == "upgrade_available":
    report = asg.migrate("main", mode="apply")
    print(f"migrated {result['from']} -> {report['final_version']}")
```

## Tests

```bash
maturin develop --release
pip install -e '.[test]'
pytest tests/
```
