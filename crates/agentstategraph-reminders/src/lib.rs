//! Agent reminder substrate for AgentStateGraph.
//!
//! Reminders are future-oriented, pull-based scheduling primitives for
//! agents. An agent (or user) creates a reminder with a due time,
//! instructions, and optional soft references to branches, memories,
//! plans, or tasks. At any checkpoint the agent calls `remind_me()` to
//! retrieve due reminders ordered by priority, then acts on them.
//!
//! ## Key concepts
//!
//! - **Pull-based**: nothing is pushed to agents; they poll `remind_me()`
//!   at natural checkpoints (session start, task completion, etc.).
//! - **Autonomous flag**: `autonomous: true` means the agent may execute
//!   without asking; `false` requires explicit user approval first.
//! - **Soft refs**: reminders may reference branches, memories, plans,
//!   or tasks by ID. Refs are advisory — the reminder is not invalidated
//!   if the referenced object is deleted; the ref is marked stale instead.
//! - **Repeating schedules**: `Schedule::Interval`, `::Daily`, `::Weekly`
//!   cause `record_execution` to automatically compute the next `due_at`
//!   and reset status to `Pending`.

pub mod error;
pub mod manager;
pub mod memory;
pub mod store;
pub mod types;

pub use error::ReminderError;
pub use manager::ReminderManager;
pub use memory::MemoryReminderStore;
pub use store::ReminderStore;
pub use types::{
    CreateReminder, ExecutionRecord, ExecutionResult, Priority, RefKind, Reminder, ReminderFilter,
    ReminderRef, ReminderStatus, Schedule,
};
