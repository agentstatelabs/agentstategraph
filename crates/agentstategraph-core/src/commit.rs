//! Commit — an immutable record linking a state tree to its history and metadata.
//!
//! This is where StateGraph diverges most from git. Beyond the standard state-root
//! and parents, a commit carries: agent identity, authority, structured intent,
//! reasoning, confidence, and tool call provenance.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::intent::{AgentId, Authority, Intent, ToolCall};
use crate::object::ObjectId;

/// An immutable record that links a state tree to its history and provenance metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Commit {
    /// Content-address of this commit (BLAKE3 hash of all other fields).
    pub id: ObjectId,
    /// Root of the state tree at this commit.
    pub state_root: ObjectId,
    /// Parent commit(s). Empty for initial commit, one for normal, two+ for merge.
    pub parents: Vec<ObjectId>,
    /// When this commit was created.
    pub timestamp: DateTime<Utc>,

    // -- Identity: who performed this action --
    /// The agent or human who made this state change.
    pub agent_id: AgentId,

    // -- Authority: who authorized this action --
    /// The authorization chain for this action.
    pub authority: Authority,

    // -- Intent: why this action was taken --
    /// Structured intent metadata.
    pub intent: Intent,

    // -- Reasoning: how the agent decided on this approach --
    /// Agent's chain-of-thought or explanation.
    pub reasoning: Option<String>,
    /// Agent's self-assessed confidence (0.0 to 1.0).
    pub confidence: Option<f64>,

    // -- Provenance: what tool calls produced this state change --
    /// Tool calls that contributed to this state change.
    pub tool_calls: Vec<ToolCall>,
}

/// Builder for creating commits. The commit ID is computed on build.
pub struct CommitBuilder {
    state_root: ObjectId,
    parents: Vec<ObjectId>,
    agent_id: AgentId,
    authority: Authority,
    intent: Intent,
    reasoning: Option<String>,
    confidence: Option<f64>,
    tool_calls: Vec<ToolCall>,
}

impl CommitBuilder {
    /// Start building a commit with the required fields.
    pub fn new(
        state_root: ObjectId,
        agent_id: impl Into<AgentId>,
        authority: Authority,
        intent: Intent,
    ) -> Self {
        Self {
            state_root,
            parents: Vec::new(),
            agent_id: agent_id.into(),
            authority,
            intent,
            reasoning: None,
            confidence: None,
            tool_calls: Vec::new(),
        }
    }

    /// Set parent commits.
    pub fn parents(mut self, parents: Vec<ObjectId>) -> Self {
        self.parents = parents;
        self
    }

    /// Set a single parent commit (most common case).
    pub fn parent(mut self, parent: ObjectId) -> Self {
        self.parents = vec![parent];
        self
    }

    /// Set the reasoning trace.
    pub fn reasoning(mut self, reasoning: impl Into<String>) -> Self {
        self.reasoning = Some(reasoning.into());
        self
    }

    /// Set the confidence score.
    pub fn confidence(mut self, confidence: f64) -> Self {
        self.confidence = Some(confidence.clamp(0.0, 1.0));
        self
    }

    /// Add tool calls.
    pub fn tool_calls(mut self, calls: Vec<ToolCall>) -> Self {
        self.tool_calls = calls;
        self
    }

    /// Build the commit, computing its content-addressed ID.
    pub fn build(self) -> Commit {
        let timestamp = Utc::now();

        // Create the commit without the ID first
        let mut commit = Commit {
            id: ObjectId::from_bytes([0u8; 32]), // placeholder
            state_root: self.state_root,
            parents: self.parents,
            timestamp,
            agent_id: self.agent_id,
            authority: self.authority,
            intent: self.intent,
            reasoning: self.reasoning,
            confidence: self.confidence,
            tool_calls: self.tool_calls,
        };

        // Compute the content-addressed ID from all fields
        let bytes = serde_json::to_vec(&CommitHashInput {
            state_root: &commit.state_root,
            parents: &commit.parents,
            timestamp: &commit.timestamp,
            agent_id: &commit.agent_id,
            authority: &commit.authority,
            intent: &commit.intent,
            reasoning: &commit.reasoning,
            confidence: &commit.confidence,
            tool_calls: &commit.tool_calls,
        })
        .expect("commit serialization should never fail");

        commit.id = ObjectId::hash(&bytes);
        commit
    }
}

/// Internal struct used for computing the commit hash.
/// Excludes the `id` field (which is what we're computing).
#[derive(Serialize)]
struct CommitHashInput<'a> {
    state_root: &'a ObjectId,
    parents: &'a Vec<ObjectId>,
    timestamp: &'a DateTime<Utc>,
    agent_id: &'a AgentId,
    authority: &'a Authority,
    intent: &'a Intent,
    reasoning: &'a Option<String>,
    confidence: &'a Option<f64>,
    tool_calls: &'a Vec<ToolCall>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intent::IntentCategory;

    fn test_intent() -> Intent {
        Intent::new(IntentCategory::Checkpoint, "test commit")
    }

    fn test_authority() -> Authority {
        Authority::simple("test-agent")
    }

    #[test]
    fn test_commit_builder() {
        let state_root = ObjectId::hash(b"test-state");
        let commit = CommitBuilder::new(state_root, "agent/test", test_authority(), test_intent())
            .reasoning("this is a test")
            .confidence(0.95)
            .build();

        assert_eq!(commit.state_root, state_root);
        assert_eq!(commit.agent_id, "agent/test");
        assert_eq!(commit.reasoning, Some("this is a test".to_string()));
        assert_eq!(commit.confidence, Some(0.95));
        assert!(commit.parents.is_empty());
    }

    #[test]
    fn test_commit_id_is_content_addressed() {
        let state_root = ObjectId::hash(b"test-state");

        // Two commits with different intents should have different IDs
        let commit1 = CommitBuilder::new(
            state_root,
            "agent/test",
            test_authority(),
            Intent::new(IntentCategory::Explore, "exploring option A"),
        )
        .build();

        let commit2 = CommitBuilder::new(
            state_root,
            "agent/test",
            test_authority(),
            Intent::new(IntentCategory::Fix, "fixing a bug"),
        )
        .build();

        assert_ne!(
            commit1.id, commit2.id,
            "different commits should have different IDs"
        );
    }

    #[test]
    fn test_commit_with_parent() {
        let parent_id = ObjectId::hash(b"parent-commit");
        let state_root = ObjectId::hash(b"child-state");

        let commit = CommitBuilder::new(state_root, "agent/test", test_authority(), test_intent())
            .parent(parent_id)
            .build();

        assert_eq!(commit.parents, vec![parent_id]);
    }

    #[test]
    fn test_confidence_clamped() {
        let state_root = ObjectId::hash(b"test");
        let commit = CommitBuilder::new(state_root, "agent/test", test_authority(), test_intent())
            .confidence(1.5) // over 1.0
            .build();

        assert_eq!(commit.confidence, Some(1.0));
    }
}
