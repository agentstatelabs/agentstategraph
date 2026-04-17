//! Path construction helpers.
//!
//! All task-store data lives under `<prefix>/<plan>/...` where `<prefix>`
//! is bound at `TaskStore::new` time. This module centralises the
//! construction so nothing else in the crate builds paths by hand.

use crate::types::TaskId;

/// Root of a plan — holds `_meta` and task entries.
pub fn plan_root(prefix: &str, plan: &str) -> String {
    format!("{}/{}", prefix, plan)
}

/// Path to a plan's `_meta` entry (the `Plan` JSON).
pub fn plan_meta(prefix: &str, plan: &str) -> String {
    format!("{}/{}/_meta", prefix, plan)
}

/// Path to a task entry inside a plan (the `Task` JSON).
pub fn task(prefix: &str, plan: &str, id: &TaskId) -> String {
    format!("{}/{}/{}", prefix, plan, id.as_str())
}

/// Reserved key under `<plan>` that holds plan-level metadata. Any entry
/// whose path segment matches this is NOT a task.
pub const META_KEY: &str = "_meta";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_compose() {
        assert_eq!(plan_root("/plans", "website-v2"), "/plans/website-v2");
        assert_eq!(plan_meta("/plans", "website-v2"), "/plans/website-v2/_meta");
        assert_eq!(
            task("/plans", "website-v2", &TaskId::new(7)),
            "/plans/website-v2/t-007"
        );
    }

    #[test]
    fn nested_prefix_works() {
        assert_eq!(
            plan_meta("/threads/tasks", "thread-1"),
            "/threads/tasks/thread-1/_meta"
        );
    }
}
