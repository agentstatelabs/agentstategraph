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
    if let Some(ref agent) = filters.agent_id {
        if &commit.agent_id != agent {
            return false;
        }
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
    if let Some(ref principal) = filters.authority_principal {
        if &commit.authority.principal != principal {
            return false;
        }
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
    if let Some(from) = filters.date_from {
        if commit.timestamp < from {
            return false;
        }
    }
    if let Some(to) = filters.date_to {
        if commit.timestamp > to {
            return false;
        }
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
}
