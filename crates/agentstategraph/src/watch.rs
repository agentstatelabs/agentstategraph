//! Watch / subscribe system — reactive agents get notified of state changes.

use std::collections::HashMap;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use agentstategraph_core::intent::Intent;
use agentstategraph_core::object::ObjectId;

/// A subscription to state changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubscriptionId(u64);

static NEXT_SUB_ID: AtomicU64 = AtomicU64::new(1);

/// Pattern for matching paths in watch subscriptions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PathPattern {
    /// Match exact path.
    Exact(String),
    /// Match any path starting with this prefix.
    Prefix(String),
    /// Match all paths.
    All,
}

impl PathPattern {
    pub fn matches(&self, path: &str) -> bool {
        match self {
            PathPattern::Exact(p) => path == p,
            PathPattern::Prefix(p) => path.starts_with(p),
            PathPattern::All => true,
        }
    }
}

/// An event emitted when watched state changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchEvent {
    pub commit_id: ObjectId,
    pub path: String,
    pub agent_id: String,
    pub intent_description: String,
    pub intent_category: String,
    pub timestamp: String,
}

/// A registered watcher.
struct Watcher {
    pattern: PathPattern,
    events: Vec<WatchEvent>,
}

/// Manages watch subscriptions.
pub struct WatchManager {
    watchers: RwLock<HashMap<SubscriptionId, Watcher>>,
}

impl Default for WatchManager {
    fn default() -> Self {
        Self::new()
    }
}

impl WatchManager {
    pub fn new() -> Self {
        Self {
            watchers: RwLock::new(HashMap::new()),
        }
    }

    /// Subscribe to state changes matching a pattern.
    pub fn subscribe(&self, pattern: PathPattern) -> SubscriptionId {
        let id = SubscriptionId(NEXT_SUB_ID.fetch_add(1, Ordering::Relaxed));
        self.watchers
            .write()
            .expect("WatchManager lock poisoned by earlier panic")
            .insert(
            id,
            Watcher {
                pattern,
                events: Vec::new(),
            },
        );
        id
    }

    /// Unsubscribe.
    pub fn unsubscribe(&self, id: SubscriptionId) -> bool {
        self.watchers
            .write()
            .expect("WatchManager lock poisoned by earlier panic")
            .remove(&id)
            .is_some()
    }

    /// Notify all matching watchers of a state change.
    /// Called internally by the Repository after each commit.
    pub fn notify(
        &self,
        commit_id: ObjectId,
        changed_paths: &[String],
        agent_id: &str,
        intent: &Intent,
    ) {
        let mut watchers = self
            .watchers
            .write()
            .expect("WatchManager lock poisoned by earlier panic");
        for watcher in watchers.values_mut() {
            for path in changed_paths {
                if watcher.pattern.matches(path) {
                    watcher.events.push(WatchEvent {
                        commit_id,
                        path: path.clone(),
                        agent_id: agent_id.to_string(),
                        intent_description: intent.description.clone(),
                        intent_category: format!("{:?}", intent.category),
                        timestamp: chrono::Utc::now().to_rfc3339(),
                    });
                }
            }
        }
    }

    /// Drain events for a subscription (returns and clears pending events).
    pub fn drain_events(&self, id: SubscriptionId) -> Vec<WatchEvent> {
        let mut watchers = self
            .watchers
            .write()
            .expect("WatchManager lock poisoned by earlier panic");
        if let Some(watcher) = watchers.get_mut(&id) {
            std::mem::take(&mut watcher.events)
        } else {
            Vec::new()
        }
    }

    /// Peek at pending events without draining.
    pub fn pending_count(&self, id: SubscriptionId) -> usize {
        self.watchers
            .read()
            .expect("WatchManager lock poisoned by earlier panic")
            .get(&id)
            .map(|w| w.events.len())
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentstategraph_core::intent::{Intent, IntentCategory};

    fn test_intent() -> Intent {
        Intent::new(IntentCategory::Refine, "test change")
    }

    #[test]
    fn test_exact_pattern() {
        let mgr = WatchManager::new();
        let sub = mgr.subscribe(PathPattern::Exact("/nodes/0/status".to_string()));

        mgr.notify(
            ObjectId::hash(b"c1"),
            &["/nodes/0/status".to_string()],
            "agent/test",
            &test_intent(),
        );

        // Shouldn't match
        mgr.notify(
            ObjectId::hash(b"c2"),
            &["/nodes/1/status".to_string()],
            "agent/test",
            &test_intent(),
        );

        let events = mgr.drain_events(sub);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].path, "/nodes/0/status");
    }

    #[test]
    fn test_prefix_pattern() {
        let mgr = WatchManager::new();
        let sub = mgr.subscribe(PathPattern::Prefix("/nodes/".to_string()));

        mgr.notify(
            ObjectId::hash(b"c1"),
            &["/nodes/0/status".to_string(), "/config/network".to_string()],
            "agent/test",
            &test_intent(),
        );

        let events = mgr.drain_events(sub);
        assert_eq!(events.len(), 1); // only /nodes/* matched
    }

    #[test]
    fn test_all_pattern() {
        let mgr = WatchManager::new();
        let sub = mgr.subscribe(PathPattern::All);

        mgr.notify(
            ObjectId::hash(b"c1"),
            &["/a".to_string(), "/b".to_string()],
            "agent/test",
            &test_intent(),
        );

        let events = mgr.drain_events(sub);
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn test_drain_clears_events() {
        let mgr = WatchManager::new();
        let sub = mgr.subscribe(PathPattern::All);

        mgr.notify(
            ObjectId::hash(b"c1"),
            &["/x".to_string()],
            "agent/test",
            &test_intent(),
        );

        assert_eq!(mgr.pending_count(sub), 1);
        let _ = mgr.drain_events(sub);
        assert_eq!(mgr.pending_count(sub), 0);
    }

    #[test]
    fn test_unsubscribe() {
        let mgr = WatchManager::new();
        let sub = mgr.subscribe(PathPattern::All);
        assert!(mgr.unsubscribe(sub));
        assert!(!mgr.unsubscribe(sub)); // already removed
    }

    use agentstategraph_core::object::ObjectId;

    #[test]
    fn test_multiple_subscribers_independent() {
        let mgr = WatchManager::new();
        let sub1 = mgr.subscribe(PathPattern::Exact("/a".to_string()));
        let sub2 = mgr.subscribe(PathPattern::Exact("/b".to_string()));

        mgr.notify(
            ObjectId::hash(b"c1"),
            &["/a".to_string(), "/b".to_string()],
            "agent/test",
            &test_intent(),
        );

        let e1 = mgr.drain_events(sub1);
        let e2 = mgr.drain_events(sub2);
        assert_eq!(e1.len(), 1, "sub1 should get /a event");
        assert_eq!(e2.len(), 1, "sub2 should get /b event");
        assert_eq!(e1[0].path, "/a");
        assert_eq!(e2[0].path, "/b");
    }

    #[test]
    fn test_events_accumulate_across_notifies() {
        let mgr = WatchManager::new();
        let sub = mgr.subscribe(PathPattern::All);

        mgr.notify(
            ObjectId::hash(b"c1"),
            &["/x".to_string()],
            "agent/test",
            &test_intent(),
        );
        mgr.notify(
            ObjectId::hash(b"c2"),
            &["/y".to_string()],
            "agent/test",
            &test_intent(),
        );

        assert_eq!(mgr.pending_count(sub), 2);
        let events = mgr.drain_events(sub);
        assert_eq!(events.len(), 2);
        assert_eq!(mgr.pending_count(sub), 0);
    }

    #[test]
    fn test_pending_count_unknown_subscription() {
        let mgr = WatchManager::new();
        let sub = mgr.subscribe(PathPattern::All);
        mgr.unsubscribe(sub);
        // Should return 0, not panic
        assert_eq!(mgr.pending_count(sub), 0);
    }

    #[test]
    fn test_drain_unknown_subscription_returns_empty() {
        let mgr = WatchManager::new();
        let sub = mgr.subscribe(PathPattern::All);
        mgr.unsubscribe(sub);
        let events = mgr.drain_events(sub);
        assert!(events.is_empty());
    }

    #[test]
    fn test_watch_event_fields_populated() {
        let mgr = WatchManager::new();
        let sub = mgr.subscribe(PathPattern::All);
        let commit = ObjectId::hash(b"c1");

        mgr.notify(commit, &["/test/path".to_string()], "agent/watcher", &test_intent());

        let events = mgr.drain_events(sub);
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert_eq!(e.commit_id, commit);
        assert_eq!(e.path, "/test/path");
        assert_eq!(e.agent_id, "agent/watcher");
        assert!(!e.intent_description.is_empty());
        assert!(!e.intent_category.is_empty());
        assert!(!e.timestamp.is_empty());
    }

    #[test]
    fn test_path_pattern_matches() {
        assert!(PathPattern::Exact("/a/b".to_string()).matches("/a/b"));
        assert!(!PathPattern::Exact("/a/b".to_string()).matches("/a/b/c"));
        assert!(PathPattern::Prefix("/a/".to_string()).matches("/a/b"));
        assert!(PathPattern::Prefix("/a/".to_string()).matches("/a/b/c"));
        assert!(!PathPattern::Prefix("/a/".to_string()).matches("/b"));
        assert!(PathPattern::All.matches("/anything"));
        assert!(PathPattern::All.matches(""));
    }
}
