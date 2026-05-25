# Example: Pull-based reminders driving a task plan

This walkthrough shows the agent-native scheduling loop: a reminder is created
now, surfaced later when an agent checks in, and its execution spawns a task in
a plan. No background timers — everything is pull-based.

All snippets are MCP tool calls. See the [Reminders](https://agentstategraph.dev/guides/reminders/)
and [Plans & Tasks](https://agentstategraph.dev/guides/plans-tasks/) guides.

## 1. Create a reminder for future work

A reminder to clean up a dev server, due later, requiring approval before it
runs (not autonomous):

```json
// agentstategraph_reminder_create
{
  "title": "Stop local web server",
  "instructions": "The dev server started for PR review may still be running. Check and terminate it.",
  "due_at": "2026-05-26T09:00:00Z",
  "priority": "high",
  "autonomous": false,
  "created_by": "agent/dev",
  "tags": ["cleanup", "server"]
}
// → { "id": "rem-0193a4f2-...", "status": "Pending" }
```

## 2. At the next checkpoint, pull what's due

When the agent next starts up, it calls `remind_me`. Past-due `Pending` items
are lazily promoted to `Due` and returned, highest priority first:

```json
// agentstategraph_reminder_remind_me
{ "created_by": "agent/dev" }
// → [ { "id": "rem-0193a4f2-...", "title": "Stop local web server",
//       "status": "Due", "priority": "High", "autonomous": false } ]
```

Because this reminder is non-autonomous, it needs approval before the agent acts:

```json
// agentstategraph_reminder_approve
{ "id": "rem-0193a4f2-...", "approved_by": "user" }
```

## 3. Turn the reminder into tracked work

Create a plan and add the cleanup as a task. `add_task` returns the assigned
task id (e.g. `t-001`); use it for the lifecycle calls. Completion requires a
typed proof.

```json
// agentstategraph_create_plan
{ "name": "ops-cleanup", "description": "Recurring housekeeping" }
```
```json
// agentstategraph_add_task
{ "plan": "ops-cleanup", "title": "Terminate stray dev server", "priority": "High", "assigned_to": "agent/dev" }
// → task "t-001" added
```
```json
// agentstategraph_start_task
{ "plan": "ops-cleanup", "task_id": "t-001" }
```
```json
// agentstategraph_complete_task
{
  "plan": "ops-cleanup",
  "task_id": "t-001",
  "proof_kind": "Text",
  "proof_value": "pkill -f 'astro dev' — confirmed no process on :4321",
  "proof_note": "Server was running; terminated."
}
```

## 4. Record the reminder execution

Recording a successful execution closes the loop. For a repeating schedule the
next due time is computed automatically; a `once` reminder transitions to
`Completed`. Link it to the task it produced:

```json
// agentstategraph_reminder_record_execution
{
  "id": "rem-0193a4f2-...",
  "agent_id": "agent/dev",
  "result": "success",
  "approved_by": "user",
  "task_id": "t-001",
  "notes": ["Stray server terminated; task t-001 completed."]
}
```

## Key takeaways

- Reminders are pull-based: durably recorded now, surfaced at the agent's next
  checkpoint via `remind_me` — no daemon required.
- `autonomous: false` keeps a human or supervisor in the loop via `approve`.
- Tasks carry a verifiable `proof`, and reminders link to the tasks they spawn
  for a complete audit trail.
