"""Named sync remotes from Python (DESIGN.md Plan G t-008)."""

import pytest

from agentstategraph_py import AgentStateGraph


def test_remotes_are_kept_listed_and_removed():
    sg = AgentStateGraph()
    sg.remote_add("hub", "https://hub.example.com", token_env="ASG_HUB_TOKEN")
    remotes = sg.remotes()
    assert remotes[0]["name"] == "hub"
    assert remotes[0]["store"] == "default"
    assert remotes[0]["token_env"] == "ASG_HUB_TOKEN"
    assert sg.remote_remove("hub") is True
    assert sg.remote_remove("hub") is False


def test_a_bad_remote_or_an_unknown_one_raises():
    sg = AgentStateGraph()
    with pytest.raises(RuntimeError):
        sg.remote_add("hub", "ftp://hub.example.com")
    with pytest.raises(RuntimeError, match="no remote is named nowhere"):
        sg.fetch("nowhere")
