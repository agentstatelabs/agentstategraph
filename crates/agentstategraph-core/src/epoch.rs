//! Epochs — bounded, sealable segments of work for lifecycle management.
//!
//! An epoch groups related commits, intents, and resolutions into a coherent
//! unit that can be sealed (made immutable), exported, and archived.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::intent::IntentId;
use crate::object::ObjectId;

/// A bounded segment of work within a StateGraph instance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Epoch {
    /// Unique identifier for this epoch.
    pub id: String,
    /// Human-readable description.
    pub description: String,
    /// The top-level intents that define this epoch's scope.
    pub root_intents: Vec<IntentId>,
    /// Current status.
    pub status: EpochStatus,
    /// When this epoch was created.
    pub created_at: DateTime<Utc>,
    /// When this epoch was sealed (if sealed).
    pub sealed_at: Option<DateTime<Utc>>,
    /// Summary filed at seal time.
    pub seal_summary: Option<String>,
    /// Merkle root of all epoch contents when sealed (tamper-evident).
    pub seal_hash: Option<ObjectId>,
    /// All commits in this epoch.
    pub commits: Vec<ObjectId>,
    /// All agents that participated.
    pub agents: Vec<String>,
    /// All branches created during this epoch.
    pub branches: Vec<String>,
    /// Queryable tags.
    pub tags: Vec<String>,
}

/// Status of an epoch.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EpochStatus {
    /// Work in progress, commits still being added.
    Active,
    /// Work complete, read-only, hash-verified.
    Sealed,
    /// Sealed and moved to cold storage, queryable via index.
    Archived,
}

/// A cross-reference between epochs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrossReference {
    /// Source epoch.
    pub from_epoch: String,
    /// Target epoch.
    pub to_epoch: String,
    /// Nature of the relationship.
    pub relationship: CrossRefType,
    /// Description of the relationship.
    pub description: String,
}

/// Types of cross-epoch relationships.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CrossRefType {
    /// This epoch's work was a follow-up from that epoch.
    FollowUp,
    /// This epoch depends on that epoch's state.
    Dependency,
    /// General relation.
    Related,
    /// This epoch reverts work from that epoch.
    Reverts,
}

/// The registry — a master index of all epochs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Registry {
    /// All epochs in this StateGraph instance.
    pub epochs: Vec<EpochEntry>,
    /// Cross-references between epochs.
    pub cross_references: Vec<CrossReference>,
}

/// A lightweight index entry for an epoch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EpochEntry {
    pub id: String,
    pub description: String,
    pub status: EpochStatus,
    pub created_at: DateTime<Utc>,
    pub sealed_at: Option<DateTime<Utc>>,
    pub root_intents: Vec<IntentId>,
    pub agents: Vec<String>,
    pub commit_count: usize,
    pub seal_hash: Option<ObjectId>,
    pub tags: Vec<String>,
}

impl Epoch {
    /// Create a new active epoch.
    pub fn new(
        id: impl Into<String>,
        description: impl Into<String>,
        root_intents: Vec<IntentId>,
    ) -> Self {
        Self {
            id: id.into(),
            description: description.into(),
            root_intents,
            status: EpochStatus::Active,
            created_at: Utc::now(),
            sealed_at: None,
            seal_summary: None,
            seal_hash: None,
            commits: Vec::new(),
            agents: Vec::new(),
            branches: Vec::new(),
            tags: Vec::new(),
        }
    }

    /// Seal this epoch, making it immutable.
    pub fn seal(&mut self, summary: String, seal_hash: ObjectId) -> Result<(), EpochError> {
        if self.status != EpochStatus::Active {
            return Err(EpochError::NotActive(self.id.clone()));
        }
        self.status = EpochStatus::Sealed;
        self.sealed_at = Some(Utc::now());
        self.seal_summary = Some(summary);
        self.seal_hash = Some(seal_hash);
        Ok(())
    }

    /// Archive this epoch.
    pub fn archive(&mut self) -> Result<(), EpochError> {
        if self.status != EpochStatus::Sealed {
            return Err(EpochError::NotSealed(self.id.clone()));
        }
        self.status = EpochStatus::Archived;
        Ok(())
    }

    /// Add a commit to this epoch.
    pub fn add_commit(&mut self, commit_id: ObjectId, agent_id: &str) -> Result<(), EpochError> {
        if self.status != EpochStatus::Active {
            return Err(EpochError::NotActive(self.id.clone()));
        }
        self.commits.push(commit_id);
        if !self.agents.contains(&agent_id.to_string()) {
            self.agents.push(agent_id.to_string());
        }
        Ok(())
    }

    /// Convert to a lightweight index entry.
    pub fn to_entry(&self) -> EpochEntry {
        EpochEntry {
            id: self.id.clone(),
            description: self.description.clone(),
            status: self.status.clone(),
            created_at: self.created_at,
            sealed_at: self.sealed_at,
            root_intents: self.root_intents.clone(),
            agents: self.agents.clone(),
            commit_count: self.commits.len(),
            seal_hash: self.seal_hash,
            tags: self.tags.clone(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EpochError {
    #[error("epoch '{0}' is not active")]
    NotActive(String),
    #[error("epoch '{0}' is not sealed")]
    NotSealed(String),
    #[error("epoch not found: {0}")]
    NotFound(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_epoch_lifecycle() {
        let mut epoch = Epoch::new("test-epoch", "Test", vec!["intent-1".to_string()]);
        assert_eq!(epoch.status, EpochStatus::Active);

        // Add commits while active
        epoch.add_commit(ObjectId::hash(b"c1"), "agent/test").unwrap();
        epoch.add_commit(ObjectId::hash(b"c2"), "agent/test").unwrap();
        assert_eq!(epoch.commits.len(), 2);

        // Seal
        epoch.seal("All done".to_string(), ObjectId::hash(b"seal")).unwrap();
        assert_eq!(epoch.status, EpochStatus::Sealed);
        assert!(epoch.sealed_at.is_some());

        // Can't add commits after seal
        assert!(epoch.add_commit(ObjectId::hash(b"c3"), "agent/test").is_err());

        // Archive
        epoch.archive().unwrap();
        assert_eq!(epoch.status, EpochStatus::Archived);
    }

    #[test]
    fn test_cannot_seal_non_active() {
        let mut epoch = Epoch::new("test", "Test", vec![]);
        epoch.seal("done".to_string(), ObjectId::hash(b"seal")).unwrap();
        assert!(epoch.seal("again".to_string(), ObjectId::hash(b"seal2")).is_err());
    }

    #[test]
    fn test_cannot_archive_non_sealed() {
        let mut epoch = Epoch::new("test", "Test", vec![]);
        assert!(epoch.archive().is_err());
    }

    #[test]
    fn test_to_entry() {
        let mut epoch = Epoch::new("test", "Test epoch", vec!["i1".to_string()]);
        epoch.tags = vec!["storage".to_string()];
        epoch.add_commit(ObjectId::hash(b"c1"), "agent/a").unwrap();

        let entry = epoch.to_entry();
        assert_eq!(entry.id, "test");
        assert_eq!(entry.commit_count, 1);
        assert_eq!(entry.tags, vec!["storage"]);
    }
}
