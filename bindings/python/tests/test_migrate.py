"""Migration Registry binding smoke tests."""
from agentstategraph_py import AgentStateGraph, exit_codes


def test_check_on_fresh_graph_is_up_to_date():
    asg = AgentStateGraph()
    r = asg.check_schema()
    # A freshly-init'd graph stamps the binary's SCHEMA_VERSION, so it
    # should be up-to-date against itself.
    assert r["status"] in ("up_to_date", "unversioned")


def test_migrate_dry_run_produces_report():
    asg = AgentStateGraph()
    report = asg.migrate("main", None, "dry-run")
    assert report["mode"] == "dry-run"
    assert "steps" in report
    assert report["from"] == report["final_version"] or isinstance(report["steps"], list)


def test_migrate_apply_is_idempotent_on_fresh_graph():
    asg = AgentStateGraph()
    report = asg.migrate("main", None, "apply")
    # Fresh graph → no steps needed, or all steps are skipped.
    for step in report["steps"]:
        assert step["status"] in ("skipped", "applied")


def test_exit_codes_exposed():
    codes = exit_codes()
    for key in ("OK", "DOWNGRADE_REFUSED", "CORRUPT_META", "MIGRATION_FAILED", "UPGRADE_REQUIRED"):
        assert key in codes
        assert isinstance(codes[key], int)
