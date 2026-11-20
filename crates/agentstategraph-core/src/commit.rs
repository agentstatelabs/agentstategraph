//! Commit — an immutable record linking a state tree to its history and metadata.
//!
//! This is where AgentStateGraph diverges most from git. Beyond the standard state-root
//! and parents, a commit carries: agent identity, authority, structured intent,
//! reasoning, confidence, and tool call provenance.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::intent::{AgentId, Authority, DeviceId, Intent, ToolCall};
use crate::object::ObjectId;

/// Maximum length (in `char`s) of `Intent::description` stored on a commit.
/// Overlong values are silently truncated on `CommitBuilder::build` — this is
/// advisory provenance, not identity, so rejecting would force the blame/log
/// path into a failure mode callers don't expect.
pub const MAX_DESCRIPTION_LEN: usize = 1024;

/// Maximum length (in `char`s) of `Commit::reasoning`. Truncated silently.
pub const MAX_REASONING_LEN: usize = 8192;

/// Maximum length (in `char`s) of `Commit::agent_id`. Truncated silently.
pub const MAX_AGENT_ID_LEN: usize = 128;

/// Maximum number of tool calls recorded on a single commit. Excess tail is
/// silently dropped on `CommitBuilder::build`.
pub const MAX_TOOL_CALLS: usize = 256;

/// Maximum length (in `char`s) of `Commit::device`. Truncated silently.
pub const MAX_DEVICE_LEN: usize = 128;

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

/// A device id as a commit stores it: control characters dropped, capped at
/// [`MAX_DEVICE_LEN`] chars. `None` when nothing printable is left, so an
/// empty device hashes like no device at all.
fn clean_device(device: &str) -> Option<String> {
    let printable: String = device
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_DEVICE_LEN)
        .collect();
    (!printable.is_empty()).then_some(printable)
}

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
    /// The device (install, machine) the change was made from, when the host
    /// knows it: CTXone sends a per-install id. Provenance only — nothing
    /// authorizes on it.
    ///
    /// Left out of the stored JSON and of the hashed fields when `None`, so a
    /// commit without a device keeps the id it had before this field existed,
    /// and readers older than 1.6 load commits that have one (they ignore it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<DeviceId>,

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

impl Commit {
    /// The content address of this commit's fields, computed the way
    /// [`CommitBuilder::build`] computes `id`. A commit is intact when this
    /// equals its `id`; sync and import recompute it for every commit they
    /// receive rather than trust the sender's `id` (DESIGN.md Plan G).
    pub fn compute_id(&self) -> ObjectId {
        let bytes = serde_json::to_vec(&CommitHashInput {
            state_root: &self.state_root,
            parents: &self.parents,
            timestamp: &self.timestamp,
            agent_id: &self.agent_id,
            device: &self.device,
            authority: &self.authority,
            intent: &self.intent,
            reasoning: &self.reasoning,
            confidence: &self.confidence,
            tool_calls: &self.tool_calls,
        })
        .expect("commit serialization should never fail");
        ObjectId::hash(&bytes)
    }

    /// Apply the same size caps `CommitBuilder::build` uses to a commit
    /// loaded from storage. Idempotent — commits already within caps are
    /// returned unchanged.
    ///
    /// `CommitBuilder::build` truncates on construction, but a `.db` file
    /// seeded out-of-band (older ASG versions without caps, or a malicious
    /// operator) may contain unbounded `description` / `reasoning` /
    /// `agent_id` / `tool_calls`. Blame/log would otherwise replay them
    /// verbatim to untrusted readers. Higher-level APIs that surface
    /// commits to callers invoke this implicitly.
    /// (security threat model v2, F3)
    ///
    /// Note: this mutates the commit in place and does NOT recompute the
    /// content-addressed `id` — a commit's `id` is a historical identifier
    /// of what was stored, and re-hashing on read would break log/blame
    /// identity. The caps here exist purely to bound what's shown to
    /// readers, not to rewrite history.
    pub fn enforce_caps(&mut self) {
        if self.agent_id.chars().count() > MAX_AGENT_ID_LEN {
            self.agent_id = truncate_chars(&self.agent_id, MAX_AGENT_ID_LEN);
        }
        if let Some(device) = self.device.as_deref() {
            let cleaned = clean_device(device);
            if cleaned.as_deref() != Some(device) {
                self.device = cleaned;
            }
        }
        if self.intent.description.chars().count() > MAX_DESCRIPTION_LEN {
            self.intent.description = truncate_chars(&self.intent.description, MAX_DESCRIPTION_LEN);
        }
        if let Some(reasoning) = self.reasoning.as_mut()
            && reasoning.chars().count() > MAX_REASONING_LEN
        {
            *reasoning = truncate_chars(reasoning, MAX_REASONING_LEN);
        }
        if self.tool_calls.len() > MAX_TOOL_CALLS {
            self.tool_calls.truncate(MAX_TOOL_CALLS);
        }
    }
}

/// Builder for creating commits. The commit ID is computed on build.
pub struct CommitBuilder {
    state_root: ObjectId,
    parents: Vec<ObjectId>,
    agent_id: AgentId,
    device: Option<DeviceId>,
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
            device: None,
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

    /// Set the device the change was made from. Control characters are
    /// dropped and the id is capped at [`MAX_DEVICE_LEN`] chars on build; an
    /// id with nothing printable left is no device.
    pub fn device(mut self, device: impl Into<DeviceId>) -> Self {
        self.device = Some(device.into());
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
    ///
    /// Silently truncates overlong provenance fields to their caps —
    /// `MAX_DESCRIPTION_LEN`, `MAX_REASONING_LEN`, `MAX_AGENT_ID_LEN`,
    /// `MAX_DEVICE_LEN`, `MAX_TOOL_CALLS`. Truncation uses `chars()` so UTF-8
    /// codepoints are never split.
    pub fn build(self) -> Commit {
        let timestamp = Utc::now();

        // Enforce length caps up front — see the module-level MAX_* constants.
        let agent_id = truncate_chars(&self.agent_id, MAX_AGENT_ID_LEN);
        let device = self.device.as_deref().and_then(clean_device);
        let mut intent = self.intent;
        intent.description = truncate_chars(&intent.description, MAX_DESCRIPTION_LEN);
        let reasoning = self
            .reasoning
            .map(|r| truncate_chars(&r, MAX_REASONING_LEN));
        let mut tool_calls = self.tool_calls;
        if tool_calls.len() > MAX_TOOL_CALLS {
            tool_calls.truncate(MAX_TOOL_CALLS);
        }

        // Create the commit without the ID first
        let mut commit = Commit {
            id: ObjectId::from_bytes([0u8; 32]), // placeholder
            state_root: self.state_root,
            parents: self.parents,
            timestamp,
            agent_id,
            device,
            authority: self.authority,
            intent,
            reasoning,
            confidence: self.confidence,
            tool_calls,
        };

        // Compute the content-addressed ID from all fields
        commit.id = commit.compute_id();
        commit
    }
}

/// Internal struct used for computing the commit hash.
/// Excludes the `id` field (which is what we're computing).
///
/// Field order and serialization are part of every commit id: change neither.
/// A new field must be skipped when unset, as `device` is, so commits made
/// without it keep their ids.
#[derive(Serialize)]
struct CommitHashInput<'a> {
    state_root: &'a ObjectId,
    parents: &'a Vec<ObjectId>,
    timestamp: &'a DateTime<Utc>,
    agent_id: &'a AgentId,
    #[serde(skip_serializing_if = "Option::is_none")]
    device: &'a Option<DeviceId>,
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
    fn compute_id_matches_build_and_survives_a_json_round_trip() {
        let commit = CommitBuilder::new(
            ObjectId::hash(b"root"),
            "agent/test",
            test_authority(),
            test_intent(),
        )
        .parent(ObjectId::hash(b"parent"))
        .reasoning("why")
        .confidence(0.3)
        .build();
        assert_eq!(commit.compute_id(), commit.id);

        // Storage keeps commits as JSON, and sync recomputes ids from what it
        // reads back, so the round trip must not change any hashed field.
        let back: Commit = serde_json::from_slice(&serde_json::to_vec(&commit).unwrap()).unwrap();
        assert_eq!(back.compute_id(), commit.id);

        let mut tampered = back;
        tampered.intent.description = "something else".into();
        assert_ne!(tampered.compute_id(), commit.id);
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
    fn test_description_truncated() {
        let state_root = ObjectId::hash(b"test");
        let long = "x".repeat(MAX_DESCRIPTION_LEN + 500);
        let commit = CommitBuilder::new(
            state_root,
            "agent/test",
            test_authority(),
            Intent::new(IntentCategory::Checkpoint, long),
        )
        .build();
        assert_eq!(
            commit.intent.description.chars().count(),
            MAX_DESCRIPTION_LEN
        );
    }

    #[test]
    fn test_reasoning_truncated() {
        let state_root = ObjectId::hash(b"test");
        let long = "r".repeat(MAX_REASONING_LEN + 10);
        let commit = CommitBuilder::new(state_root, "agent/test", test_authority(), test_intent())
            .reasoning(long)
            .build();
        assert_eq!(
            commit.reasoning.as_ref().unwrap().chars().count(),
            MAX_REASONING_LEN
        );
    }

    #[test]
    fn test_agent_id_truncated() {
        let state_root = ObjectId::hash(b"test");
        let long = "a".repeat(MAX_AGENT_ID_LEN + 50);
        let commit = CommitBuilder::new(state_root, long, test_authority(), test_intent()).build();
        assert_eq!(commit.agent_id.chars().count(), MAX_AGENT_ID_LEN);
    }

    #[test]
    fn test_tool_calls_truncated() {
        let state_root = ObjectId::hash(b"test");
        let calls: Vec<ToolCall> = (0..(MAX_TOOL_CALLS + 20))
            .map(|i| ToolCall {
                tool_name: format!("tool_{}", i),
                arguments: serde_json::json!({}),
                result: None,
                timestamp: Utc::now(),
            })
            .collect();
        let commit = CommitBuilder::new(state_root, "agent/test", test_authority(), test_intent())
            .tool_calls(calls)
            .build();
        assert_eq!(commit.tool_calls.len(), MAX_TOOL_CALLS);
    }

    #[test]
    fn test_truncation_preserves_utf8_boundaries() {
        let state_root = ObjectId::hash(b"test");
        // 4-byte UTF-8 emoji; chars().take() must not split them.
        let emoji_flood = "🎉".repeat(MAX_DESCRIPTION_LEN + 10);
        let commit = CommitBuilder::new(
            state_root,
            "agent/test",
            test_authority(),
            Intent::new(IntentCategory::Checkpoint, emoji_flood),
        )
        .build();
        assert_eq!(
            commit.intent.description.chars().count(),
            MAX_DESCRIPTION_LEN
        );
        // If we'd split a codepoint, the String would not round-trip through char count equal to byte/4.
        assert!(
            commit
                .intent
                .description
                .is_char_boundary(commit.intent.description.len())
        );
    }

    #[test]
    fn test_enforce_caps_truncates_bypassed_builder() {
        // Simulate a commit loaded from a `.db` written out-of-band
        // (direct struct construction bypasses `CommitBuilder::build`).
        let big_reasoning = "r".repeat(100_000); // ~100 KB
        let big_desc = "d".repeat(MAX_DESCRIPTION_LEN * 4);
        let big_agent = "a".repeat(MAX_AGENT_ID_LEN * 10);
        let big_tools: Vec<ToolCall> = (0..(MAX_TOOL_CALLS * 2))
            .map(|i| ToolCall {
                tool_name: format!("t{}", i),
                arguments: serde_json::json!({}),
                result: None,
                timestamp: Utc::now(),
            })
            .collect();

        let mut commit = Commit {
            id: ObjectId::hash(b"test"),
            state_root: ObjectId::hash(b"state"),
            parents: vec![],
            timestamp: Utc::now(),
            agent_id: big_agent,
            device: Some(format!("dev\u{7}{}", "d".repeat(MAX_DEVICE_LEN * 2))),
            authority: test_authority(),
            intent: Intent::new(IntentCategory::Checkpoint, big_desc),
            reasoning: Some(big_reasoning),
            confidence: Some(0.5),
            tool_calls: big_tools,
        };

        commit.enforce_caps();

        assert_eq!(commit.agent_id.chars().count(), MAX_AGENT_ID_LEN);
        assert_eq!(
            commit.intent.description.chars().count(),
            MAX_DESCRIPTION_LEN
        );
        assert_eq!(
            commit.reasoning.as_ref().unwrap().chars().count(),
            MAX_REASONING_LEN
        );
        assert_eq!(commit.tool_calls.len(), MAX_TOOL_CALLS);
        let device = commit.device.as_deref().unwrap();
        assert_eq!(device.chars().count(), MAX_DEVICE_LEN);
        assert!(device.starts_with("devd"), "control character dropped");
    }

    #[test]
    fn test_enforce_caps_is_idempotent() {
        // A commit already within caps must be unchanged.
        let commit_orig = CommitBuilder::new(
            ObjectId::hash(b"s"),
            "agent/test",
            test_authority(),
            test_intent(),
        )
        .reasoning("short")
        .device("0123456789abcdef0123456789abcdef")
        .build();
        let mut commit = commit_orig.clone();
        commit.enforce_caps();
        assert_eq!(commit.agent_id, commit_orig.agent_id);
        assert_eq!(commit.intent.description, commit_orig.intent.description);
        assert_eq!(commit.reasoning, commit_orig.reasoning);
        assert_eq!(commit.device, commit_orig.device);
        assert_eq!(commit.tool_calls.len(), commit_orig.tool_calls.len());
    }

    /// The hashed fields exactly as they were before `device` existed (1.5.x).
    #[derive(Serialize)]
    struct HashInputBeforeDevice<'a> {
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

    fn hash_input_before_device(c: &Commit) -> Vec<u8> {
        serde_json::to_vec(&HashInputBeforeDevice {
            state_root: &c.state_root,
            parents: &c.parents,
            timestamp: &c.timestamp,
            agent_id: &c.agent_id,
            authority: &c.authority,
            intent: &c.intent,
            reasoning: &c.reasoning,
            confidence: &c.confidence,
            tool_calls: &c.tool_calls,
        })
        .unwrap()
    }

    fn hash_input(c: &Commit) -> Vec<u8> {
        serde_json::to_vec(&CommitHashInput {
            state_root: &c.state_root,
            parents: &c.parents,
            timestamp: &c.timestamp,
            agent_id: &c.agent_id,
            device: &c.device,
            authority: &c.authority,
            intent: &c.intent,
            reasoning: &c.reasoning,
            confidence: &c.confidence,
            tool_calls: &c.tool_calls,
        })
        .unwrap()
    }

    #[test]
    fn a_commit_without_a_device_hashes_as_before() {
        let commit = CommitBuilder::new(
            ObjectId::hash(b"root"),
            "agent/test",
            test_authority(),
            test_intent(),
        )
        .parent(ObjectId::hash(b"parent"))
        .reasoning("why")
        .confidence(0.4)
        .tool_calls(vec![ToolCall {
            tool_name: "set".into(),
            arguments: serde_json::json!({"path": "/a"}),
            result: None,
            timestamp: Utc::now(),
        }])
        .build();
        assert_eq!(commit.device, None);
        assert_eq!(hash_input(&commit), hash_input_before_device(&commit));
        assert_eq!(
            commit.id,
            ObjectId::hash(&hash_input_before_device(&commit)),
            "the id a 1.5 build would give this commit"
        );
        // And the stored JSON has no device key at all.
        let stored = serde_json::to_value(&commit).unwrap();
        assert!(stored.get("device").is_none(), "{stored}");
    }

    #[test]
    fn a_device_is_part_of_the_id() {
        let commit = CommitBuilder::new(
            ObjectId::hash(b"root"),
            "agent/test",
            test_authority(),
            test_intent(),
        )
        .device("0123456789abcdef0123456789abcdef")
        .build();
        assert_eq!(
            commit.device.as_deref(),
            Some("0123456789abcdef0123456789abcdef")
        );
        assert_eq!(commit.compute_id(), commit.id);
        assert_ne!(hash_input(&commit), hash_input_before_device(&commit));

        let mut other = commit.clone();
        other.device = Some("fedcba9876543210fedcba9876543210".into());
        assert_ne!(other.compute_id(), commit.id);
        other.device = None;
        assert_ne!(other.compute_id(), commit.id);

        // The device survives the JSON round trip storage and sync use.
        let back: Commit = serde_json::from_slice(&serde_json::to_vec(&commit).unwrap()).unwrap();
        assert_eq!(back.device, commit.device);
        assert_eq!(back.compute_id(), commit.id);
    }

    #[test]
    fn a_commit_stored_before_devices_still_loads() {
        // A commit as 1.5.x stored it: no `device` key.
        let commit = CommitBuilder::new(
            ObjectId::hash(b"root"),
            "agent/test",
            test_authority(),
            test_intent(),
        )
        .build();
        let mut stored = serde_json::to_value(&commit).unwrap();
        stored.as_object_mut().unwrap().remove("device");
        let back: Commit = serde_json::from_value(stored).unwrap();
        assert_eq!(back.device, None);
        assert_eq!(back.compute_id(), commit.id);
    }

    #[test]
    fn a_reader_without_devices_ignores_one() {
        // What a 1.5.x reader does with a commit that has a device: serde
        // skips the unknown key (nothing in Commit denies unknown fields).
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct CommitBeforeDevice {
            id: ObjectId,
            state_root: ObjectId,
            parents: Vec<ObjectId>,
            timestamp: DateTime<Utc>,
            agent_id: AgentId,
            authority: Authority,
            intent: Intent,
            reasoning: Option<String>,
            confidence: Option<f64>,
            tool_calls: Vec<ToolCall>,
        }
        let commit = CommitBuilder::new(
            ObjectId::hash(b"root"),
            "agent/test",
            test_authority(),
            test_intent(),
        )
        .device("laptop-1")
        .build();
        let old: CommitBeforeDevice =
            serde_json::from_slice(&serde_json::to_vec(&commit).unwrap()).unwrap();
        assert_eq!(old.id, commit.id);
        assert_eq!(old.agent_id, commit.agent_id);
    }

    #[test]
    fn device_is_capped_and_cleaned() {
        let build = |device: &str| {
            CommitBuilder::new(
                ObjectId::hash(b"s"),
                "agent/test",
                test_authority(),
                test_intent(),
            )
            .device(device)
            .build()
        };
        let long = build(&"é".repeat(MAX_DEVICE_LEN + 7));
        assert_eq!(
            long.device.as_deref().unwrap().chars().count(),
            MAX_DEVICE_LEN
        );
        assert_eq!(
            build("dev\nice\u{0}\t\u{1b}[31m").device.as_deref(),
            Some("device[31m")
        );
        // Nothing printable left is no device, and hashes like none.
        let blank = build("\n\r\t");
        assert_eq!(blank.device, None);
        assert_eq!(hash_input(&blank), hash_input_before_device(&blank));
        assert_eq!(build("").device, None);
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
