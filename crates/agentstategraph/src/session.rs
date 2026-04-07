//! Agent sessions — working contexts for sub-agent orchestration.
//!
//! Sessions formalize the parent-child agent relationship. A lead agent
//! delegates work by creating scoped sessions for sub-agents, each with
//! their own branch and restricted path access.

use std::collections::HashMap;
use std::sync::RwLock;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use agentstategraph_core::intent::{AgentId, IntentId, SessionId};
use agentstategraph_core::object::ObjectId;

/// An active agent session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub agent_id: AgentId,
    pub working_branch: String,
    pub head: ObjectId,
    /// Who spawned this session.
    pub parent_session: Option<SessionId>,
    /// The intent this session was created to fulfill.
    pub delegated_intent: Option<IntentId>,
    /// Who to report back to.
    pub report_to: Option<String>,
    /// Path scope restriction (if set, agent can only modify paths under this prefix).
    pub path_scope: Option<String>,
    /// When this session was created.
    pub created_at: DateTime<Utc>,
}

/// Manages active sessions.
pub struct SessionManager {
    sessions: RwLock<HashMap<SessionId, Session>>,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("session not found: {0}")]
    NotFound(String),
    #[error("path '{path}' is outside session scope '{scope}'")]
    OutOfScope { path: String, scope: String },
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
        }
    }

    /// Create a new session.
    pub fn create(
        &self,
        agent_id: &str,
        working_branch: &str,
        head: ObjectId,
        parent_session: Option<SessionId>,
        delegated_intent: Option<IntentId>,
        report_to: Option<String>,
        path_scope: Option<String>,
    ) -> Session {
        let id = uuid::Uuid::new_v4().to_string();
        let session = Session {
            id: id.clone(),
            agent_id: agent_id.to_string(),
            working_branch: working_branch.to_string(),
            head,
            parent_session,
            delegated_intent,
            report_to,
            path_scope,
            created_at: Utc::now(),
        };
        self.sessions.write().unwrap().insert(id, session.clone());
        session
    }

    /// Get a session by ID.
    pub fn get(&self, id: &str) -> Option<Session> {
        self.sessions.read().unwrap().get(id).cloned()
    }

    /// Update a session's head.
    pub fn update_head(&self, id: &str, head: ObjectId) -> Result<(), SessionError> {
        let mut sessions = self.sessions.write().unwrap();
        let session = sessions
            .get_mut(id)
            .ok_or_else(|| SessionError::NotFound(id.to_string()))?;
        session.head = head;
        Ok(())
    }

    /// List all sessions, optionally filtered by agent.
    pub fn list(&self, agent_filter: Option<&str>) -> Vec<Session> {
        let sessions = self.sessions.read().unwrap();
        sessions
            .values()
            .filter(|s| agent_filter.map(|f| s.agent_id == f).unwrap_or(true))
            .cloned()
            .collect()
    }

    /// List child sessions of a parent.
    pub fn children(&self, parent_id: &str) -> Vec<Session> {
        let sessions = self.sessions.read().unwrap();
        sessions
            .values()
            .filter(|s| s.parent_session.as_deref() == Some(parent_id))
            .cloned()
            .collect()
    }

    /// Remove a session.
    pub fn remove(&self, id: &str) -> Option<Session> {
        self.sessions.write().unwrap().remove(id)
    }

    /// Check if a path is within a session's scope.
    pub fn check_scope(session: &Session, path: &str) -> Result<(), SessionError> {
        if let Some(ref scope) = session.path_scope {
            if !path.starts_with(scope) {
                return Err(SessionError::OutOfScope {
                    path: path.to_string(),
                    scope: scope.clone(),
                });
            }
        }
        Ok(())
    }

    /// Count active sessions.
    pub fn count(&self) -> usize {
        self.sessions.read().unwrap().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_and_get_session() {
        let mgr = SessionManager::new();
        let session = mgr.create(
            "agent/planner",
            "agents/planner/workspace",
            ObjectId::hash(b"head"),
            None,
            None,
            None,
            None,
        );

        let retrieved = mgr.get(&session.id).unwrap();
        assert_eq!(retrieved.agent_id, "agent/planner");
    }

    #[test]
    fn test_parent_child_sessions() {
        let mgr = SessionManager::new();
        let parent = mgr.create(
            "agent/orchestrator",
            "agents/orchestrator/workspace",
            ObjectId::hash(b"head"),
            None,
            Some("intent-001".to_string()),
            None,
            None,
        );

        let child1 = mgr.create(
            "agent/storage",
            "agents/storage/workspace",
            ObjectId::hash(b"head"),
            Some(parent.id.clone()),
            Some("intent-002".to_string()),
            Some("agent/orchestrator".to_string()),
            Some("/config/storage".to_string()),
        );

        let child2 = mgr.create(
            "agent/network",
            "agents/network/workspace",
            ObjectId::hash(b"head"),
            Some(parent.id.clone()),
            Some("intent-003".to_string()),
            Some("agent/orchestrator".to_string()),
            Some("/config/network".to_string()),
        );

        let children = mgr.children(&parent.id);
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
            created_at: Utc::now(),
        };

        // Within scope
        assert!(SessionManager::check_scope(&session, "/config/storage/type").is_ok());
        assert!(SessionManager::check_scope(&session, "/config/storage").is_ok());

        // Out of scope
        assert!(SessionManager::check_scope(&session, "/config/network/subnet").is_err());
        assert!(SessionManager::check_scope(&session, "/nodes/0").is_err());
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
            created_at: Utc::now(),
        };

        assert!(SessionManager::check_scope(&session, "/anything/at/all").is_ok());
    }

    #[test]
    fn test_list_by_agent() {
        let mgr = SessionManager::new();
        mgr.create(
            "agent/a",
            "br/a",
            ObjectId::hash(b"h"),
            None,
            None,
            None,
            None,
        );
        mgr.create(
            "agent/b",
            "br/b",
            ObjectId::hash(b"h"),
            None,
            None,
            None,
            None,
        );
        mgr.create(
            "agent/a",
            "br/a2",
            ObjectId::hash(b"h"),
            None,
            None,
            None,
            None,
        );

        assert_eq!(mgr.list(Some("agent/a")).len(), 2);
        assert_eq!(mgr.list(Some("agent/b")).len(), 1);
        assert_eq!(mgr.list(None).len(), 3);
    }
}
