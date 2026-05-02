//! Unified query interface — one API for querying state, commits, intents, and epochs.
//!
//! All filters are optional and combined with AND. Simple queries use
//! one or two filters. Complex queries combine many.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::commit::Commit;

/// What to query — the primary dimension.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum QueryTarget {
    /// Current state values.
    State,
    /// Commit history.
    Commits,
    /// Intent metadata.
    Intents,
    /// Agent activity.
    Agents,
    /// Epoch records.
    Epochs,
}

/// Composable query filters. All optional, combined with AND.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QueryFilters {
    /// Path pattern (e.g., "/nodes/*", "/config/network/**").
    pub path: Option<String>,
    /// Agent ID filter.
    pub agent_id: Option<String>,
    /// Intent category filter.
    pub intent_category: Option<String>,
    /// Intent tags (all must match).
    pub tags: Option<Vec<String>>,
    /// Authority principal filter.
    pub authority_principal: Option<String>,
    /// Full-text search in reasoning traces.
    pub reasoning_contains: Option<String>,
    /// Confidence range [min, max].
    pub confidence_range: Option<(f64, f64)>,
    /// Intent status filter.
    pub intent_status: Option<String>,
    /// Outcome filter.
    pub outcome: Option<String>,
    /// Date range [start, end].
    pub date_from: Option<DateTime<Utc>>,
    pub date_to: Option<DateTime<Utc>>,
    /// Only results with deviations.
    pub has_deviations: Option<bool>,
}

/// Output control for queries.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QueryOptions {
    /// Max results.
    pub limit: Option<usize>,
    /// Pagination offset.
    pub offset: Option<usize>,
    /// Sort field.
    pub order_by: Option<String>,
}

/// A query request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Query {
    pub target: QueryTarget,
    pub ref_name: Option<String>,
    pub filters: QueryFilters,
    pub options: QueryOptions,
}

/// Apply filters to a list of commits, returning only matches.
pub fn filter_commits(commits: &[Commit], filters: &QueryFilters) -> Vec<Commit> {
    commits
        .iter()
        .filter(|c| matches_filters(c, filters))
        .cloned()
        .collect()
}

/// Check if a commit matches all specified filters.
pub fn matches_filters(commit: &Commit, filters: &QueryFilters) -> bool {
    // Agent filter
    if let Some(ref agent) = filters.agent_id
        && &commit.agent_id != agent
    {
        return false;
    }

    // Intent category filter
    if let Some(ref category) = filters.intent_category {
        let commit_cat = format!("{:?}", commit.intent.category);
        if !commit_cat.eq_ignore_ascii_case(category) {
            return false;
        }
    }

    // Tags filter (all must match)
    if let Some(ref tags) = filters.tags {
        for tag in tags {
            if !commit.intent.tags.contains(tag) {
                return false;
            }
        }
    }

    // Authority principal filter
    if let Some(ref principal) = filters.authority_principal
        && &commit.authority.principal != principal
    {
        return false;
    }

    // Reasoning contains (full-text search)
    if let Some(ref query) = filters.reasoning_contains {
        let query_lower = query.to_lowercase();
        let matches = commit
            .reasoning
            .as_ref()
            .map(|r| r.to_lowercase().contains(&query_lower))
            .unwrap_or(false)
            || commit
                .intent
                .description
                .to_lowercase()
                .contains(&query_lower);
        if !matches {
            return false;
        }
    }

    // Confidence range
    if let Some((min, max)) = filters.confidence_range {
        match commit.confidence {
            Some(c) if c >= min && c <= max => {}
            Some(_) => return false,
            None => return false,
        }
    }

    // Intent status filter
    if let Some(ref status) = filters.intent_status {
        let commit_status = format!("{:?}", commit.intent.lifecycle.status);
        if !commit_status.eq_ignore_ascii_case(status) {
            return false;
        }
    }

    // Date range
    if let Some(from) = filters.date_from
        && commit.timestamp < from
    {
        return false;
    }
    if let Some(to) = filters.date_to
        && commit.timestamp > to
    {
        return false;
    }

    // Has deviations
    if let Some(true) = filters.has_deviations {
        let has = commit
            .intent
            .lifecycle
            .resolution
            .as_ref()
            .map(|r| !r.deviations.is_empty())
            .unwrap_or(false);
        if !has {
            return false;
        }
    }

    true
}

/// Blame entry — who last modified a value and why.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlameEntry {
    pub path: String,
    pub commit_id: String,
    pub agent_id: String,
    pub intent_category: String,
    pub intent_description: String,
    pub reasoning: Option<String>,
    pub timestamp: DateTime<Utc>,
    /// True if this commit's timestamp is less than or equal to at least
    /// one of its parents' timestamps. Indicates a possible clock rewind
    /// (or a legitimate concurrent commit in a DAG merge). Always check
    /// alongside the commit id, not instead of it — a well-intentioned
    /// skew is not an attack, but a persistent pattern is a signal.
    /// (security threat model v3+, V4)
    #[serde(default)]
    pub timestamp_anomaly: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commit::CommitBuilder;
    use crate::intent::{Authority, Intent, IntentCategory};
    use crate::object::ObjectId;

    fn test_commit(agent: &str, category: IntentCategory, desc: &str) -> Commit {
        CommitBuilder::new(
            ObjectId::hash(b"state"),
            agent,
            Authority::simple(agent),
            Intent::new(category, desc),
        )
        .build()
    }

    fn test_commit_with_reasoning(agent: &str, desc: &str, reasoning: &str) -> Commit {
        CommitBuilder::new(
            ObjectId::hash(b"state"),
            agent,
            Authority::simple(agent),
            Intent::new(IntentCategory::Explore, desc),
        )
        .reasoning(reasoning)
        .confidence(0.8)
        .build()
    }

    #[test]
    fn test_filter_by_agent() {
        let commits = vec![
            test_commit("agent/a", IntentCategory::Explore, "by a"),
            test_commit("agent/b", IntentCategory::Explore, "by b"),
            test_commit("agent/a", IntentCategory::Fix, "fix by a"),
        ];

        let filtered = filter_commits(
            &commits,
            &QueryFilters {
                agent_id: Some("agent/a".to_string()),
                ..Default::default()
            },
        );
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn test_filter_by_category() {
        let commits = vec![
            test_commit("agent/a", IntentCategory::Explore, "explore"),
            test_commit("agent/a", IntentCategory::Fix, "fix"),
            test_commit("agent/a", IntentCategory::Explore, "explore 2"),
        ];

        let filtered = filter_commits(
            &commits,
            &QueryFilters {
                intent_category: Some("Explore".to_string()),
                ..Default::default()
            },
        );
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn test_filter_by_reasoning_contains() {
        let commits = vec![
            test_commit_with_reasoning("a", "storage", "NFS is better for small clusters"),
            test_commit_with_reasoning("a", "network", "10GbE bonding configured"),
            test_commit_with_reasoning("a", "gpu", "Memory controller issue on node 3"),
        ];

        let filtered = filter_commits(
            &commits,
            &QueryFilters {
                reasoning_contains: Some("memory controller".to_string()),
                ..Default::default()
            },
        );
        assert_eq!(filtered.len(), 1);
        assert!(filtered[0].intent.description.contains("gpu"));
    }

    #[test]
    fn test_filter_by_confidence_range() {
        let commits = vec![
            {
                let mut c = test_commit("a", IntentCategory::Explore, "high");
                c.confidence = Some(0.9);
                c
            },
            {
                let mut c = test_commit("a", IntentCategory::Explore, "low");
                c.confidence = Some(0.3);
                c
            },
            {
                let mut c = test_commit("a", IntentCategory::Explore, "mid");
                c.confidence = Some(0.6);
                c
            },
        ];

        let filtered = filter_commits(
            &commits,
            &QueryFilters {
                confidence_range: Some((0.0, 0.5)),
                ..Default::default()
            },
        );
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].intent.description, "low");
    }

    #[test]
    fn test_combined_filters() {
        let commits = vec![
            test_commit_with_reasoning("agent/planner", "storage explore", "trying NFS"),
            test_commit_with_reasoning("agent/planner", "network fix", "fixing DNS"),
            test_commit_with_reasoning("agent/monitor", "health check", "node healthy"),
        ];

        let filtered = filter_commits(
            &commits,
            &QueryFilters {
                agent_id: Some("agent/planner".to_string()),
                reasoning_contains: Some("NFS".to_string()),
                ..Default::default()
            },
        );
        assert_eq!(filtered.len(), 1);
    }

    // --- Empty filters pass everything ---

    #[test]
    fn test_empty_filters_returns_all() {
        let commits = vec![
            test_commit("a", IntentCategory::Explore, "x"),
            test_commit("b", IntentCategory::Fix, "y"),
            test_commit("c", IntentCategory::Checkpoint, "z"),
        ];
        let filtered = filter_commits(&commits, &QueryFilters::default());
        assert_eq!(filtered.len(), 3);
    }

    // --- Tags filter ---

    #[test]
    fn test_filter_by_tags_all_must_match() {
        use crate::intent::Intent;
        let make = |tags: &[&str]| {
            CommitBuilder::new(
                ObjectId::hash(b"s"),
                "a",
                Authority::simple("a"),
                Intent::new(IntentCategory::Explore, "x")
                    .with_tags(tags.iter().map(|s| s.to_string()).collect()),
            )
            .build()
        };

        let commits = vec![
            make(&["gpu", "node-3"]),
            make(&["gpu"]),
            make(&["cpu", "node-3"]),
        ];

        // Both tags must match — only the first commit qualifies
        let filtered = filter_commits(
            &commits,
            &QueryFilters {
                tags: Some(vec!["gpu".to_string(), "node-3".to_string()]),
                ..Default::default()
            },
        );
        assert_eq!(filtered.len(), 1);
    }

    // --- Authority principal filter ---

    #[test]
    fn test_filter_by_authority_principal() {
        let commits = vec![
            CommitBuilder::new(
                ObjectId::hash(b"s"),
                "a",
                Authority::simple("human/alice"),
                Intent::new(IntentCategory::Explore, "by alice"),
            )
            .build(),
            CommitBuilder::new(
                ObjectId::hash(b"s"),
                "a",
                Authority::simple("human/bob"),
                Intent::new(IntentCategory::Explore, "by bob"),
            )
            .build(),
        ];

        let filtered = filter_commits(
            &commits,
            &QueryFilters {
                authority_principal: Some("human/alice".to_string()),
                ..Default::default()
            },
        );
        assert_eq!(filtered.len(), 1);
        assert!(filtered[0].intent.description.contains("alice"));
    }

    // --- Confidence range: None confidence excluded ---

    #[test]
    fn test_confidence_range_excludes_none_confidence() {
        let commits = vec![
            {
                let mut c = test_commit("a", IntentCategory::Explore, "has confidence");
                c.confidence = Some(0.8);
                c
            },
            // No confidence set — should be excluded by any confidence_range filter
            test_commit("a", IntentCategory::Explore, "no confidence"),
        ];

        let filtered = filter_commits(
            &commits,
            &QueryFilters {
                confidence_range: Some((0.0, 1.0)),
                ..Default::default()
            },
        );
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].intent.description, "has confidence");
    }

    // --- Intent category filter: case-insensitive ---

    #[test]
    fn test_category_filter_case_insensitive() {
        let commits = vec![
            test_commit("a", IntentCategory::Explore, "x"),
            test_commit("a", IntentCategory::Fix, "y"),
        ];

        // "explore" lowercase should match "Explore" category
        let filtered = filter_commits(
            &commits,
            &QueryFilters {
                intent_category: Some("explore".to_string()),
                ..Default::default()
            },
        );
        assert_eq!(filtered.len(), 1);
    }

    // --- Date range ---

    #[test]
    fn test_filter_by_date_range() {
        use chrono::Duration;
        let now = Utc::now();

        let mut past = test_commit("a", IntentCategory::Explore, "old");
        past.timestamp = now - Duration::days(10);

        let mut recent = test_commit("a", IntentCategory::Explore, "recent");
        recent.timestamp = now - Duration::days(1);

        let mut future = test_commit("a", IntentCategory::Explore, "future");
        future.timestamp = now + Duration::days(1);

        let commits = vec![past.clone(), recent.clone(), future.clone()];

        // Only commits within last 7 days
        let from = now - Duration::days(7);
        let to = now + Duration::minutes(1);

        let filtered = filter_commits(
            &commits,
            &QueryFilters {
                date_from: Some(from),
                date_to: Some(to),
                ..Default::default()
            },
        );
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].intent.description, "recent");
    }

    // --- has_deviations ---

    #[test]
    fn test_filter_by_has_deviations() {
        use crate::intent::{Deviation, DeviationImpact, Intent, Outcome, Resolution};

        let make_with_deviation = |desc: &str| {
            let mut intent = Intent::new(IntentCategory::Explore, desc);
            intent.lifecycle.resolution = Some(Resolution {
                summary: "completed with deviation".into(),
                outcome: Outcome::PartiallyFulfilled,
                deviations: vec![Deviation {
                    description: "unexpected value".into(),
                    reason: "state diverged".into(),
                    impact: DeviationImpact::Medium,
                    follow_up: None,
                }],
                commits: Vec::new(),
                branches_explored: Vec::new(),
                confidence: 0.7,
            });
            CommitBuilder::new(
                ObjectId::hash(desc.as_bytes()),
                "a",
                Authority::simple("a"),
                intent,
            )
            .build()
        };

        let commits = vec![
            make_with_deviation("has deviation"),
            test_commit("a", IntentCategory::Explore, "no deviation"),
        ];

        let filtered = filter_commits(
            &commits,
            &QueryFilters {
                has_deviations: Some(true),
                ..Default::default()
            },
        );
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].intent.description, "has deviation");
    }

    // --- Serialization ---

    #[test]
    fn test_query_filters_serializes_and_deserializes() {
        let f = QueryFilters {
            path: Some("/nodes/*".into()),
            agent_id: Some("agent/a".into()),
            confidence_range: Some((0.5, 0.9)),
            ..Default::default()
        };
        let json = serde_json::to_string(&f).unwrap();
        let back: QueryFilters = serde_json::from_str(&json).unwrap();
        assert_eq!(back.path.as_deref(), Some("/nodes/*"));
        assert_eq!(back.confidence_range, Some((0.5, 0.9)));
    }

    #[test]
    fn test_query_target_round_trips() {
        for t in [
            QueryTarget::State,
            QueryTarget::Commits,
            QueryTarget::Intents,
            QueryTarget::Agents,
            QueryTarget::Epochs,
        ] {
            let j = serde_json::to_value(&t).unwrap();
            let back: QueryTarget = serde_json::from_value(j).unwrap();
            assert_eq!(back, t);
        }
    }
}
