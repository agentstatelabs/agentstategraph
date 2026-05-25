---
title: Epochs
description: Bound a unit of work, seal it into a tamper-evident snapshot, then archive or export it.
---

An **epoch** is a bounded, sealable segment of work — an incident response, a release, a quarter — that groups the commits, agents, branches, and root intents belonging to it. Epochs are how you draw a line around "everything that happened here" and freeze it into something an auditor can trust.

## The lifecycle

`Active` → `Sealed` → `Archived`.

- **Seal** computes the set of commits reachable from `main`'s tip, stores a Merkle **seal hash**, and makes the epoch immutable. The seal hash is what makes it *tamper-evident*: any later alteration of a sealed commit breaks the hash. A guard rejects ref updates that would orphan sealed commits.
- **Strict mode** (`epoch_seal_strict`, via `with_epoch_seal_strict(true)` or `ASG_EPOCH_SEAL_STRICT`) turns a seal-orphaning update to `main` into a hard `EpochSealViolated` error instead of a warning. Non-`main` refs are exempt.
- **Archive** moves a sealed epoch to cold storage. It stays fully queryable.
- **Export** produces a self-contained JSON audit bundle — the epoch plus the full commit records — that you can hand to someone outside the system. Active epochs cannot be exported; only sealed or archived ones.

Rehydration reconstructs an epoch's commit list from storage on read, so the bounded set is always available even after restart.

## Why it matters

When the actor making changes is an agent fleet, "trust us, here's what happened" isn't good enough. A sealed epoch is a cryptographically verifiable record: this exact set of commits, with this intent and provenance, frozen at this moment. Exporting it turns your live state graph into a portable, immutable artifact for compliance, post-incident review, or handoff.

## MCP tools

| Tool | Purpose |
|------|---------|
| `agentstategraph_create_epoch` | Create an epoch to group related work |
| `agentstategraph_seal_epoch` | Seal an epoch (read-only, tamper-evident) |
| `agentstategraph_archive_epoch` | Move a sealed epoch to archived |
| `agentstategraph_export_epoch` | Export a sealed/archived epoch as a JSON audit bundle |
| `agentstategraph_enter_epoch` / `agentstategraph_exit_epoch` | Set / clear the active epoch |
| `agentstategraph_list_epochs` | List epochs with status, dates, and commit counts |

See the [MCP Tools reference](/reference/mcp-tools/#epochs) for parameters and examples.
