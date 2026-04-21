"""Cross-binding policy parity runner — Python side.

§7 of the 0.7.0-beta.1 plan. Loads the shared fixture
`spec/policy_parity_fixture.json`, seeds the policies, ratifies them,
and asserts the same `decision.kind` (and matched_policy prefix) as
every other binding's runner. All runners share this contract:
Rust reference, Python, TypeScript, Go, WASM, and C FFI.
"""
import json
import os

import pytest

from agentstategraph_py import AgentStateGraph, PolicyStore


def _fixture_path() -> str:
    # This file lives at bindings/python/tests/test_parity.py; the
    # fixture lives at <repo>/spec/policy_parity_fixture.json.
    here = os.path.dirname(os.path.abspath(__file__))
    return os.path.normpath(os.path.join(here, "..", "..", "..", "spec", "policy_parity_fixture.json"))


@pytest.fixture
def fixture():
    with open(_fixture_path(), "r", encoding="utf-8") as f:
        return json.load(f)


def test_parity_fixture_matches_python_binding(fixture):
    prefix = fixture.get("prefix", "/policies")
    agent_id = fixture.get("agent_id", "parity-runner")
    ref = fixture.get("ref", "main")

    asg = AgentStateGraph()
    ps = PolicyStore(asg, prefix, agent_id)

    for pol in fixture["policies"]:
        ps.propose(ref, pol)
    for r in fixture["ratify"]:
        ps.ratify(ref, r["path"], r["ratifier"], r["reasoning"])

    for entry in fixture["change_proposals"]:
        label = entry.get("label", "<unlabelled>")
        expected = entry["expected_decision_kind"]
        d = ps.evaluate_change(ref, entry["proposal"])
        assert d["kind"] == expected, f"{label}: got {d}"
        if "expected_matched_policy_prefix" in entry:
            matched = d.get("matched_policy") or ""
            assert matched.startswith(entry["expected_matched_policy_prefix"]), (
                f"{label}: matched_policy {matched!r} should start with "
                f"{entry['expected_matched_policy_prefix']!r}"
            )

    for entry in fixture["evaluate"]:
        label = entry.get("label", "<unlabelled>")
        expected = entry["expected_decision_kind"]
        d = ps.evaluate(ref, entry["situation"], entry["action"], entry["agent_id"])
        assert d["kind"] == expected, f"{label}: got {d}"
