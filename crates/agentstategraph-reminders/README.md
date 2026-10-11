# agentstategraph-reminders

Agent reminder substrate for AgentStateGraph: future-oriented, pull-based
scheduling for agents.

- **Pull-based.** Nothing is pushed; agents call `remind_me()` at natural
  checkpoints and get due reminders back in priority order.
- **Autonomous flag.** `autonomous: true` lets the agent act without asking;
  `false` requires user approval first.
- **Soft references.** Reminders can point at branches, memories, plans or
  tasks; a reference to a deleted object is marked stale rather than breaking
  the reminder.
- **Repeating schedules.** `Interval`, `Daily` and `Weekly` schedules compute
  the next due time when an execution is recorded.

Part of [AgentStateGraph](https://github.com/agentstatelabs/agentstategraph). Most applications depend on the
[`agentstategraph`](https://crates.io/crates/agentstategraph) crate, which
re-exports the core types and storage backends.

## License

Licensed under either of
[Apache License, Version 2.0](https://github.com/agentstatelabs/agentstategraph/blob/main/LICENSE-APACHE)
or [MIT license](https://github.com/agentstatelabs/agentstategraph/blob/main/LICENSE-MIT),
at your option.
