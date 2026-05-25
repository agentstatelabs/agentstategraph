---
title: Reminders
description: Pull-based scheduling for agents — no background timers, priority-ordered, with approval gating.
---

Agents don't have a cron daemon. They wake up, do work, and stop. So AgentStateGraph's reminders are **pull-based**: nothing is pushed and no timer fires in the background. Instead, an agent calls `remind_me()` at natural checkpoints — session start, a task transition, a branch switch — and receives everything that is currently due, ordered by priority.

This fits the agent execution model exactly: future work is durably recorded now, and surfaced the next time an agent is in a position to act on it.

## How it works

Create a reminder with a due time, an optional repeating schedule, a priority, and instructions. At a checkpoint, `remind_me()` lazily promotes past-due items to `Due` and returns all due items (highest priority first). The agent acts, then records the execution result.

- **Priority** — `Critical` (1) through `Minimal` (5). `remind_me()` orders by priority, then due date.
- **Schedules** — `Once`, `Interval`, `Daily`, or `Weekly`. After a successful execution of a repeating reminder, the next due time is computed and the reminder resets automatically.
- **Approval gating** — `autonomous: true` lets the agent execute immediately. `autonomous: false` moves the reminder to `AwaitingPermission`; it needs an explicit `approve()` before it can run. This is how you keep a human (or a supervising agent) in the loop for sensitive work.
- **Soft refs** — a reminder can point at a branch, memory, plan, task, state path, or external resource. The label is captured at creation time, so the reminder stays meaningful even if the target is later renamed or deleted. Stale refs never invalidate the reminder.
- **Execution history** — every run records start/end times, the executing agent, the result, and an optional task id linking to work it created.

Status lifecycle: `Pending` → `Due` → `InProgress` → `Completed`, with `Snoozed`, `AwaitingPermission`, and `Cancelled` as side states.

## Storage

Reminders persist through the `ReminderStore` trait, and `Repository` implements it directly — so reminders ride on whatever backend the repository already uses. SQLite and Postgres provide durable `reminders` tables; in-memory and IndexedDB implement the trait as well. No separate service to run.

## MCP tools

| Tool | Purpose |
|------|---------|
| `agentstategraph_reminder_create` | Create a reminder (optional schedule / approval / refs) |
| `agentstategraph_reminder_list` | List reminders with filters |
| `agentstategraph_reminder_remind_me` | Get all currently due reminders, by priority |
| `agentstategraph_reminder_snooze` | Defer a reminder until later |
| `agentstategraph_reminder_approve` | Approve a non-autonomous reminder for execution |
| `agentstategraph_reminder_cancel` | Cancel a reminder permanently |
| `agentstategraph_reminder_record_execution` | Record the result of an execution |

See the [MCP Tools reference](/reference/mcp-tools/#reminders-7-tools) for parameters and examples.
