#!/usr/bin/env python3
"""
Python Agent Reference Implementation — StateGraph in a Python AI workflow.

This shows how a Python agent (e.g., LangChain, CrewAI, or standalone)
uses StateGraph for structured state management with full provenance.

Setup:
    cd bindings/python
    python3 -m venv .venv && source .venv/bin/activate
    pip install maturin && maturin develop --release

Run:
    python3 examples/python_agent.py
"""

from agentstategraph_py import StateGraph


def main():
    print("=== Python Agent Reference Implementation ===\n")

    # ─── 1. Create a repository ────────────────────────────────────
    # In-memory for demo. Use StateGraph("state.db") for SQLite persistence.
    sg = StateGraph()
    print("✓ Repository initialized (in-memory)\n")

    # ─── 2. Set initial state with intent ──────────────────────────
    sg.set("/project/name", "ml-pipeline", "Initialize project",
           category="Checkpoint", agent="agent/setup")

    sg.set_json("/project/config", {
        "model": "llama-3",
        "epochs": 100,
        "batch_size": 32,
        "learning_rate": 0.001,
    }, "Set training configuration",
       category="Checkpoint",
       agent="agent/trainer",
       reasoning="Standard hyperparameters for LLaMA fine-tuning")

    print("✓ Initial state set\n")

    # ─── 3. Branch to explore alternatives ─────────────────────────
    sg.branch("explore/higher-lr")
    sg.set("/project/config/learning_rate", 0.01,
           "Try higher learning rate",
           ref="explore/higher-lr",
           category="Explore",
           agent="agent/hyperopt",
           reasoning="Higher LR might converge faster for this dataset",
           confidence=0.6)

    sg.branch("explore/larger-batch")
    sg.set("/project/config/batch_size", 128,
           "Try larger batch size",
           ref="explore/larger-batch",
           category="Explore",
           agent="agent/hyperopt",
           reasoning="Larger batch = more stable gradients, better GPU utilization",
           confidence=0.75)

    print("✓ Two exploration branches created\n")

    # ─── 4. Compare branches ──────────────────────────────────────
    diff_lr = sg.diff("main", "explore/higher-lr")
    diff_batch = sg.diff("main", "explore/larger-batch")
    print(f"  higher-lr changes: {len(diff_lr)}")
    for d in diff_lr:
        print(f"    {d['op']}: {d['path']} → {d.get('new', '')}")
    print(f"  larger-batch changes: {len(diff_batch)}")
    for d in diff_batch:
        print(f"    {d['op']}: {d['path']} → {d.get('new', '')}")

    # ─── 5. Speculation: try approaches without committing ─────────
    print("\n--- Speculative execution ---\n")

    spec_adam = sg.speculate(label="Adam optimizer")
    spec_sgd = sg.speculate(label="SGD with momentum")

    sg.spec_set(spec_adam, "/project/config/optimizer", "adam")
    sg.spec_set(spec_sgd, "/project/config/optimizer", "sgd")

    print(f"  Adam: {sg.spec_get(spec_adam, '/project/config/optimizer')}")
    print(f"  SGD:  {sg.spec_get(spec_sgd, '/project/config/optimizer')}")

    # Pick Adam
    sg.commit_speculation(spec_adam, "Selected Adam optimizer",
                          category="Refine",
                          reasoning="Adam converges faster for transformer fine-tuning",
                          confidence=0.85)
    sg.discard_speculation(spec_sgd)
    print("  ✓ Committed Adam, discarded SGD\n")

    # ─── 6. Merge the larger batch branch ──────────────────────────
    sg.merge("explore/larger-batch", description="Adopt larger batch size",
             reasoning="GPU utilization was only 40% with batch_size=32")
    print("✓ Merged larger-batch into main\n")

    # ─── 7. Query: find all exploration commits ────────────────────
    print("--- Query: all Explore intents ---\n")
    explores = sg.query(intent_category="Explore")
    for e in explores:
        print(f"  [{e['intent']['category']}] {e['intent']['description']}")
        if e.get('reasoning'):
            print(f"    reasoning: {e['reasoning']}")
        if e.get('confidence') is not None:
            print(f"    confidence: {e['confidence']:.0%}")
        print()

    # ─── 8. Blame: who set the optimizer? ──────────────────────────
    print("--- Blame ---\n")
    blame = sg.blame("/project/config/optimizer")
    print(f"  Who set /project/config/optimizer?")
    print(f"    agent: {blame['agent_id']}")
    print(f"    why: {blame['intent_description']}")
    if blame.get('reasoning'):
        print(f"    reasoning: {blame['reasoning']}")
    print()

    # ─── 9. Epochs: group work for compliance ─────────────────────
    sg.create_epoch("2026-Q2-training-setup", "ML training pipeline setup",
                    ["intent-training"])
    print("✓ Epoch created\n")

    # ─── 10. Full audit trail ──────────────────────────────────────
    print("--- Full commit log ---\n")
    log = sg.log(limit=20)
    for entry in reversed(log):
        intent = entry['intent']
        print(f"  {entry['id']} [{intent['category']}] {intent['description']}")
        if entry.get('reasoning'):
            short = entry['reasoning'][:60] + "..." if len(entry['reasoning']) > 60 else entry['reasoning']
            print(f"    → {short}")

    print(f"\n=== Complete! {len(log)} commits with full provenance ===")


if __name__ == "__main__":
    main()
