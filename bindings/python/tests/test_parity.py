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

    # 5. (0.7.5 §6) Optional extra_policies + ratify_extra + tenant/external
    #    evaluate blocks. Use .get() so fixtures without these keys still pass.
    for pol in fixture.get("extra_policies", []) or []:
        ps.propose(ref, pol)
    for r in fixture.get("ratify_extra", []) or []:
        ps.ratify(ref, r["path"], r["ratifier"], r["reasoning"])

    for entry in fixture.get("tenant_evaluate", []) or []:
        label = entry.get("label", "<unlabelled>")
        expected = entry["expected_decision_kind"]
        tenant = entry.get("tenant_filter")
        d = ps.evaluate(
            ref,
            entry["situation"],
            entry["action"],
            entry["agent_id"],
            tenant_filter=tenant,
        )
        assert d["kind"] == expected, f"tenant {label}: got {d}"
        if "expected_matched_policy_prefix" in entry:
            matched = d.get("matched_policy") or ""
            assert matched.startswith(entry["expected_matched_policy_prefix"]), (
                f"tenant {label}: matched_policy {matched!r} should start with "
                f"{entry['expected_matched_policy_prefix']!r}"
            )

    for entry in fixture.get("external_evaluate", []) or []:
        label = entry.get("label", "<unlabelled>")
        expected = entry["expected_decision_kind"]
        # No external runner registered → policy with external_evaluator set
        # is skipped, falling through to no_policy_match.
        d = ps.evaluate(ref, entry["situation"], entry["action"], entry["agent_id"])
        assert d["kind"] == expected, f"external {label}: got {d}"
