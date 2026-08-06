#!/usr/bin/env python3
"""Fail when a release has not explicitly reviewed every binding surface."""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "bindings" / "capabilities.json"


def fail(message: str) -> None:
    print(f"binding-capabilities: ERROR: {message}", file=sys.stderr)
    raise SystemExit(1)


data = json.loads(MANIFEST.read_text())
cargo = (ROOT / "Cargo.toml").read_text()
match = re.search(r'^version = "([^"]+)"$', cargo, re.MULTILINE)
if not match:
    fail("cannot read workspace version")
version = match.group(1)
if data.get("reviewed_core_version") != version:
    fail(
        f"bindings were reviewed for {data.get('reviewed_core_version')}, not {version}; "
        "audit every binding and update bindings/capabilities.json"
    )

capabilities = set(data.get("capabilities", []))
if not capabilities:
    fail("capability list is empty")

bindings = data.get("bindings", {})
expected = {"rust", "c", "swift", "python", "typescript", "wasm", "go", "dotnet"}
if set(bindings) != expected:
    fail(f"binding set must be exactly {sorted(expected)}")

for name, binding in bindings.items():
    source = ROOT / binding["source"]
    if not source.exists():
        fail(f"{name}: source does not exist: {binding['source']}")
    buckets = [set(binding.get(key, [])) for key in ("full", "partial", "unavailable")]
    classified = set().union(*buckets)
    if classified != capabilities:
        missing = sorted(capabilities - classified)
        extra = sorted(classified - capabilities)
        fail(f"{name}: incomplete classification; missing={missing}, extra={extra}")
    if sum(len(bucket) for bucket in buckets) != len(classified):
        fail(f"{name}: a capability appears in more than one status bucket")

operations = data.get("advanced_abi_operations", [])
if len(operations) != len(set(operations)) or not operations:
    fail("advanced_abi_operations must be non-empty and unique")

ffi = (ROOT / "crates/agentstategraph-ffi/src/lib.rs").read_text()
swift = (ROOT / "bindings/swift/Sources/AgentStateGraph/AdvancedRepository.swift").read_text()
for operation in operations:
    if ffi.count(f'"{operation}"') < 2:
        fail(f"advanced ABI operation is declared but not dispatched: {operation}")
    if f'"{operation}"' not in swift:
        fail(f"Swift does not wrap advanced ABI operation: {operation}")

symbols = (
    "agentstategraph_repository_capabilities",
    "agentstategraph_fork_namespace",
    "agentstategraph_repository_call",
)
for relative in (
    "bindings/go/agentstategraph.h",
    "bindings/swift/Sources/CAgentStateGraph/include/agentstategraph.h",
):
    header = (ROOT / relative).read_text()
    for symbol in symbols:
        if symbol not in header:
            fail(f"{relative} is missing {symbol}")

print(f"binding-capabilities: OK ({version}, {len(bindings)} bindings, {len(operations)} ABI operations)")
for name, binding in bindings.items():
    print(
        f"  {name:10} {binding['tier']:17} "
        f"full={len(binding['full']):2} partial={len(binding['partial']):2} "
        f"unavailable={len(binding['unavailable']):2}"
    )
