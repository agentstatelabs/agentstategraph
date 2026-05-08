//! Agent sessions — working contexts for sub-agent orchestration.
//!
//! The `Session` type itself lives in `agentstategraph-core` so storage
//! backends can implement `SessionStore` directly. This module keeps
//! the user-facing `SessionManager` and the `check_scope` helper, and
//! delegates all storage to the `SessionStore` trait.

use chrono::Utc;

use agentstategraph_core::intent::{AgentId, IntentId, SessionId};
use agentstategraph_core::object::ObjectId;
pub use agentstategraph_core::session::{Session, SessionStatus};
#[allow(unused_imports)]
use agentstategraph_storage::SessionStore;
use agentstategraph_storage::{Storage, StorageError};

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("session not found: {0}")]
    NotFound(String),
    #[error("path '{path}' is outside session scope '{scope}'")]
    OutOfScope { path: String, scope: String },
    #[error("storage error: {0}")]
    Storage(#[from] StorageError),
}

/// Optional fields for [`SessionManager::create`].
#[derive(Default)]
pub struct CreateSessionParams {
    pub parent_session: Option<SessionId>,
    pub delegated_intent: Option<IntentId>,
    pub report_to: Option<String>,
    pub path_scope: Option<String>,
}

/// Manages active sessions.
///
/// Backed by a `SessionStore` so sessions survive process restart.
/// This struct is a thin wrapper: creation, end-of-life and lookup all
/// route through storage.
pub struct SessionManager<'a> {
    storage: &'a dyn Storage,
}

impl<'a> SessionManager<'a> {
    pub fn new(storage: &'a dyn Storage) -> Self {
        Self { storage }
    }

    /// Create a new session and persist it.
    pub fn create(
        &self,
        agent_id: &str,
        working_branch: &str,
        head: ObjectId,
        params: CreateSessionParams,
    ) -> Result<Session, SessionError> {
        let id = uuid::Uuid::new_v4().to_string();
        let session = Session {
            id: id.clone(),
            agent_id: agent_id.to_string(),
            working_branch: working_branch.to_string(),
            head,
            parent_session: params.parent_session,
            delegated_intent: params.delegated_intent,
            report_to: params.report_to,
            path_scope: params.path_scope,
            scope_tenant: None,
            status: SessionStatus::Active,
            created_at: Utc::now(),
            ended_at: None,
        };
        self.storage.create_session(&session)?;
        Ok(session)
    }

    /// Get a session by ID.
    pub fn get(&self, id: &str) -> Result<Option<Session>, SessionError> {
        Ok(self.storage.get_session(id)?)
    }

    /// Update a session's head pointer. Persisted via a rewrite of the
    /// session row (create-or-replace semantics).
    pub fn update_head(&self, id: &str, head: ObjectId) -> Result<(), SessionError> {
        let mut session = self
            .storage
            .get_session(id)?
            .ok_or_else(|| SessionError::NotFound(id.to_string()))?;
        session.head = head;
        // create_session uses INSERT OR REPLACE in SQLite / map.insert in
        // memory, so it doubles as an update.
        self.storage.create_session(&session)?;
        Ok(())
    }

    /// List all sessions, optionally filtered by agent.
    pub fn list(&self, agent_filter: Option<&str>) -> Result<Vec<Session>, SessionError> {
        Ok(self.storage.list_sessions(agent_filter)?)
    }

    /// List child sessions of a parent.
    pub fn children(&self, parent_id: &str) -> Result<Vec<Session>, SessionError> {
        let all = self.storage.list_sessions(None)?;
        Ok(all
            .into_iter()
            .filter(|s| s.parent_session.as_deref() == Some(parent_id))
            .collect())
    }

    /// End a session. Preferred over `remove` for audit purposes.
    pub fn end(&self, id: &str, status: SessionStatus) -> Result<(), SessionError> {
        self.storage.end_session(id, status, Utc::now())?;
        Ok(())
    }

    /// Count sessions.
    pub fn count(&self) -> Result<usize, SessionError> {
        Ok(self.storage.list_sessions(None)?.len())
    }
}

/// Check if a path is within a session's scope. Free function so it
/// can be used with a borrowed `Session` without going through the
/// manager.
pub fn check_scope(session: &Session, path: &str) -> Result<(), SessionError> {
    if let Some(ref scope) = session.path_scope
        && !path.starts_with(scope)
    {
        return Err(SessionError::OutOfScope {
            path: path.to_string(),
            scope: scope.clone(),
        });
    }
    Ok(())
}

/// Back-compat alias — matches the old `SessionManager::check_scope`
/// associated-function style while delegating to the free function.
impl SessionManager<'_> {
    pub fn check_scope(session: &Session, path: &str) -> Result<(), SessionError> {
        check_scope(session, path)
    }
}

// Convenience for callers to construct an `AgentId` string inline.
#[doc(hidden)]
pub fn _sanity_agent_id() -> AgentId {
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentstategraph_storage::SqliteStorage;

    fn mgr_store() -> SqliteStorage {
        SqliteStorage::in_memory().expect("in-memory sqlite")
    }

    /// Build a minimal Session for tests. Optional/new fields default to None/Active.
    /// Update this helper when Session gains fields so call sites stay stable.
    pub fn make_session(id: &str, agent_id: &str) -> Session {
        Session {
            id: id.to_string(),
            agent_id: agent_id.to_string(),
            working_branch: format!("agents/{}/workspace", agent_id),
            head: agentstategraph_core::ObjectId::hash(id.as_bytes()),
            parent_session: None,
            delegated_intent: None,
            report_to: None,
            path_scope: None,
            scope_tenant: None,
            status: SessionStatus::Active,
            created_at: Utc::now(),
            ended_at: None,
        }
    }

    #[test]
    fn test_create_and_get_session() {
        let store = mgr_store();
        let mgr = SessionManager::new(&store);
        let session = mgr
            .create(
                "agent/planner",
                "agents/planner/workspace",
                ObjectId::hash(b"head"),
                CreateSessionParams::default(),
            )
            .unwrap();

        let retrieved = mgr.get(&session.id).unwrap().unwrap();
        assert_eq!(retrieved.agent_id, "agent/planner");
    }

    #[test]
    fn test_parent_child_sessions() {
        let store = mgr_store();
        let mgr = SessionManager::new(&store);
        let parent = mgr
            .create(
                "agent/orchestrator",
                "agents/orchestrator/workspace",
                ObjectId::hash(b"head"),
                CreateSessionParams {
                    delegated_intent: Some("intent-001".to_string()),
                    ..Default::default()
                },
            )
            .unwrap();

        mgr.create(
            "agent/storage",
            "agents/storage/workspace",
            ObjectId::hash(b"head"),
            CreateSessionParams {
                parent_session: Some(parent.id.clone()),
                delegated_intent: Some("intent-002".to_string()),
                report_to: Some("agent/orchestrator".to_string()),
                path_scope: Some("/config/storage".to_string()),
            },
        )
        .unwrap();

        mgr.create(
            "agent/network",
            "agents/network/workspace",
            ObjectId::hash(b"head"),
            CreateSessionParams {
                parent_session: Some(parent.id.clone()),
                delegated_intent: Some("intent-003".to_string()),
                report_to: Some("agent/orchestrator".to_string()),
                path_scope: Some("/config/network".to_string()),
            },
        )
        .unwrap();

        let children = mgr.children(&parent.id).unwrap();
        assert_eq!(children.len(), 2);
    }

    #[test]
    fn test_path_scope_enforcement() {
        let session = Session {
            id: "test".to_string(),
            agent_id: "agent/storage".to_string(),
            working_branch: "agents/storage/workspace".to_string(),
            head: ObjectId::hash(b"head"),
            parent_session: None,
            delegated_intent: None,
            report_to: None,
            path_scope: Some("/config/storage".to_string()),
            scope_tenant: None,
            status: SessionStatus::Active,
            created_at: Utc::now(),
            ended_at: None,
        };

        assert!(check_scope(&session, "/config/storage/type").is_ok());
        assert!(check_scope(&session, "/config/storage").is_ok());
        assert!(check_scope(&session, "/config/network/subnet").is_err());
        assert!(check_scope(&session, "/nodes/0").is_err());
    }

    #[test]
    fn test_no_scope_allows_all() {
        let session = Session {
            id: "test".to_string(),
            agent_id: "agent/admin".to_string(),
            working_branch: "main".to_string(),
            head: ObjectId::hash(b"head"),
            parent_session: None,
            delegated_intent: None,
            report_to: None,
            path_scope: None,
            scope_tenant: None,
            status: SessionStatus::Active,
            created_at: Utc::now(),
            ended_at: None,
        };
        assert!(check_scope(&session, "/anything/at/all").is_ok());
    }

    #[test]
    fn test_list_by_agent() {
        let store = mgr_store();
        let mgr = SessionManager::new(&store);
        mgr.create("agent/a", "br/a", ObjectId::hash(b"h"), CreateSessionParams::default())
            .unwrap();
        mgr.create("agent/b", "br/b", ObjectId::hash(b"h"), CreateSessionParams::default())
            .unwrap();
        mgr.create("agent/a", "br/a2", ObjectId::hash(b"h"), CreateSessionParams::default())
            .unwrap();

        assert_eq!(mgr.list(Some("agent/a")).unwrap().len(), 2);
        assert_eq!(mgr.list(Some("agent/b")).unwrap().len(), 1);
        assert_eq!(mgr.list(None).unwrap().len(), 3);
    }
}
