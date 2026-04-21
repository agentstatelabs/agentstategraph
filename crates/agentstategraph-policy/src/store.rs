//! `PolicyStore` — handle bound to a `Repository` + path prefix.
//!
//! All policy operations go through this type. Writes commit with
//! `IntentCategory::Custom("policy-propose" | "policy-ratify" |
//! "policy-supersede")` so policy activity is natively filterable in
//! the log and blame queries.

use std::sync::Arc;

use agentstategraph::{CommitOptions, Repository};
use agentstategraph_core::IntentCategory;
use chrono::Utc;

use crate::error::PolicyError;
use crate::evaluator;
use crate::paths;
use crate::selector::Situation;
use crate::types::{ChangeProposal, Decision, Policy};

/// Handle bound to a `Repository` + path prefix. Mirrors the
/// `agentstategraph-tasks` `TaskStore` pattern.
pub struct PolicyStore {
    repo: Arc<Repository>,
    prefix: String,
    agent_id: String,
}

impl PolicyStore {
    pub fn new(
        repo: Arc<Repository>,
        prefix: impl Into<String>,
        agent_id: impl Into<String>,
    ) -> Self {
        let mut prefix = prefix.into();
        if prefix.ends_with('/') {
            prefix.pop();
        }
        Self {
            repo,
            prefix,
            agent_id: agent_id.into(),
        }
    }

    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    // -----------------------------------------------------------------
    // Write operations
    // -----------------------------------------------------------------

    /// Write a proposed policy (not yet ratified). Fails if an entry
    /// already lives at the same path — supersede instead.
    ///
    /// The `path`, `version`, `proposed_by`, and `proposed_at` fields
    /// are normalized/overridden on write: callers pass a `Policy` with
    /// `path` set to the intended logical path (slashes, with or without
    /// leading `/`), and the store writes version = 1, `proposed_by =
    /// agent_id`, `proposed_at = now`, `ratified_by = None`.
    pub fn propose(&self, ref_name: &str, mut policy: Policy) -> Result<String, PolicyError> {
        let normalized = paths::normalize(&policy.path)?;
        if self.exists(ref_name, &normalized)? {
            return Err(PolicyError::AlreadyExists(normalized));
        }

        policy.path = normalized.clone();
        policy.version = 1;
        policy.proposed_by = self.agent_id.clone();
        policy.proposed_at = Utc::now();
        policy.ratified_by = None;
        policy.ratified_at = None;
        policy.ratification_reasoning = None;
        policy.supersedes = None;
        if policy.active_from.timestamp() == 0 {
            policy.active_from = Utc::now();
        }

        let path = paths::active(&self.prefix, &normalized);
        let value = serde_json::to_value(&policy)?;
        self.repo.set_json(
            ref_name,
            &path,
            &value,
            self.commit_opts(
                "policy-propose",
                format!("Propose policy {}", policy.handle()),
            ),
        )?;
        Ok(policy.handle())
    }

    /// Ratify an unratified proposal at `path`. Fails if the policy
    /// does not exist, is already ratified, or the ratifier / reasoning
    /// are empty.
    pub fn ratify(
        &self,
        ref_name: &str,
        path: &str,
        ratifier: &str,
        reasoning: &str,
    ) -> Result<(), PolicyError> {
        if ratifier.trim().is_empty() {
            return Err(PolicyError::Invalid("ratifier required".into()));
        }
        let normalized = paths::normalize(path)?;
        let mut policy = self.load_active(ref_name, &normalized)?;
        if policy.is_ratified() {
            return Err(PolicyError::AlreadyRatified(policy.handle()));
        }
        policy.ratified_by = Some(ratifier.to_string());
        policy.ratified_at = Some(Utc::now());
        policy.ratification_reasoning = if reasoning.is_empty() {
            None
        } else {
            Some(reasoning.to_string())
        };

        let active_path = paths::active(&self.prefix, &normalized);
        let value = serde_json::to_value(&policy)?;
        self.repo.set_json(
            ref_name,
            &active_path,
            &value,
            self.commit_opts(
                "policy-ratify",
                format!("Ratify policy {} by {}", policy.handle(), ratifier),
            ),
        )?;
        Ok(())
    }

    /// Replace the active policy at `path` with `new_policy`. The old
    /// active version is moved to `history/<version>` and the new one
    /// is written at the active path with `version = old + 1` and
    /// `supersedes = "<path>@<old_version>"`.
    ///
    /// The new policy is written as already ratified iff its
    /// `ratified_by` is `Some(_)` — callers that want propose-then-
    /// ratify semantics on a supersede can leave it `None` and call
    /// `ratify` separately.
    pub fn supersede(
        &self,
        ref_name: &str,
        path: &str,
        mut new_policy: Policy,
    ) -> Result<String, PolicyError> {
        let normalized = paths::normalize(path)?;
        let old = self.load_active(ref_name, &normalized)?;

        new_policy.path = normalized.clone();
        new_policy.version = old.version + 1;
        new_policy.proposed_by = self.agent_id.clone();
        new_policy.proposed_at = Utc::now();
        new_policy.supersedes = Some(old.handle());
        if new_policy.active_from.timestamp() == 0 {
            new_policy.active_from = Utc::now();
        }

        let history_path = paths::historical(&self.prefix, &normalized, old.version);
        let active_path = paths::active(&self.prefix, &normalized);
        let old_value = serde_json::to_value(&old)?;
        let new_value = serde_json::to_value(&new_policy)?;

        let handle = self
            .repo
            .speculate(ref_name, Some(format!("Supersede {}", normalized)))?;
        self.repo.spec_set_json(handle, &history_path, &old_value)?;
        self.repo.spec_set_json(handle, &active_path, &new_value)?;
        self.repo.commit_speculation(
            handle,
            self.commit_opts(
                "policy-supersede",
                format!("Supersede {} → {}", old.handle(), new_policy.handle()),
            ),
        )?;
        Ok(new_policy.handle())
    }

    // -----------------------------------------------------------------
    // Read operations
    // -----------------------------------------------------------------

    /// Fetch the active (ratified or proposed) policy at `path`, or a
    /// pinned historical version when `version` is `Some(_)`.
    pub fn get(
        &self,
        ref_name: &str,
        path: &str,
        version: Option<u64>,
    ) -> Result<Policy, PolicyError> {
        let normalized = paths::normalize(path)?;
        match version {
            None => self.load_active(ref_name, &normalized),
            Some(v) => {
                let current = self.load_active(ref_name, &normalized)?;
                if current.version == v {
                    return Ok(current);
                }
                let p = paths::historical(&self.prefix, &normalized, v);
                let value = self.repo.get_json(ref_name, &p).map_err(|e| {
                    if is_path_not_found(&e) {
                        PolicyError::NotFound(format!("{}@{}", normalized, v))
                    } else {
                        e.into()
                    }
                })?;
                Ok(serde_json::from_value(value)?)
            }
        }
    }

    /// List every policy (active versions only) whose path starts with
    /// `prefix_filter` (path form without leading slash). `None` lists
    /// all policies under the store prefix.
    pub fn list(
        &self,
        ref_name: &str,
        prefix_filter: Option<&str>,
    ) -> Result<Vec<Policy>, PolicyError> {
        let leaves = match self.repo.list_paths(ref_name, &self.prefix, None) {
            Ok(v) => v,
            Err(e) if is_path_not_found(&e) => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let store_prefix_with_slash = format!("{}/", self.prefix);
        let meta_marker = format!("/{}/", paths::META_KEY);
        let history_marker = format!("/{}/", paths::HISTORY_KEY);
        let mut policy_paths: Vec<String> = Vec::new();
        for leaf in &leaves {
            let Some(rel) = leaf.strip_prefix(&store_prefix_with_slash) else {
                continue;
            };
            // Skip history entries entirely.
            if rel.contains(&history_marker) {
                continue;
            }
            // Policies are stored as JSON objects at `<policy_path>/_meta`,
            // so every leaf for the policy lives at
            // `<policy_path>/_meta/<field...>`. Extract the policy path
            // by taking everything before `/_meta/`.
            let Some(idx) = rel.find(&meta_marker) else {
                continue;
            };
            let policy_rel = &rel[..idx];
            if policy_rel.is_empty() {
                continue;
            }
            if let Some(filter) = prefix_filter {
                let filter = filter.trim_start_matches('/');
                if !policy_rel.starts_with(filter) {
                    continue;
                }
            }
            policy_paths.push(policy_rel.to_string());
        }
        policy_paths.sort();
        policy_paths.dedup();

        let mut out = Vec::with_capacity(policy_paths.len());
        for p in policy_paths {
            match self.load_active(ref_name, &p) {
                Ok(policy) => out.push(policy),
                Err(PolicyError::NotFound(_)) => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    /// List active policies (ratified only).
    pub fn active(
        &self,
        ref_name: &str,
        prefix_filter: Option<&str>,
    ) -> Result<Vec<Policy>, PolicyError> {
        Ok(self
            .list(ref_name, prefix_filter)?
            .into_iter()
            .filter(|p| p.is_ratified())
            .collect())
    }

    /// Walk the supersedes chain. Returned oldest-first (version 1 →
    /// current).
    pub fn history(&self, ref_name: &str, path: &str) -> Result<Vec<Policy>, PolicyError> {
        let normalized = paths::normalize(path)?;
        let current = self.load_active(ref_name, &normalized)?;
        let mut versions = vec![current.clone()];
        let mut v = current.version;
        while v > 1 {
            v -= 1;
            let hp = paths::historical(&self.prefix, &normalized, v);
            let value = self.repo.get_json(ref_name, &hp).map_err(|e| {
                if is_path_not_found(&e) {
                    PolicyError::NotFound(format!("{}@{}", normalized, v))
                } else {
                    PolicyError::from(e)
                }
            })?;
            let policy: Policy = serde_json::from_value(value)?;
            versions.push(policy);
        }
        versions.reverse();
        Ok(versions)
    }

    /// Every ratified policy whose `situation_selector` matches `situation`.
    pub fn policies_for_situation(
        &self,
        ref_name: &str,
        situation: &Situation,
    ) -> Result<Vec<Policy>, PolicyError> {
        Ok(self
            .active(ref_name, None)?
            .into_iter()
            .filter(|p| p.situation_selector.matches(situation))
            .collect())
    }

    // -----------------------------------------------------------------
    // Evaluation
    // -----------------------------------------------------------------

    /// Authorization evaluation — POLICY_V1.md §5.
    pub fn evaluate(
        &self,
        ref_name: &str,
        situation: &Situation,
        action: &str,
        agent_id: &str,
    ) -> Result<Decision, PolicyError> {
        let matched = self.policies_for_situation(ref_name, situation)?;
        let refs: Vec<&Policy> = matched.iter().collect();
        Ok(evaluator::evaluate_matched(&refs, action, agent_id))
    }

    /// Change-proposal evaluation — POLICY_V1.md §22.2.
    pub fn evaluate_change(
        &self,
        ref_name: &str,
        proposal: &ChangeProposal,
    ) -> Result<Decision, PolicyError> {
        let actives = self.active(ref_name, None)?;
        let refs: Vec<&Policy> = actives.iter().collect();
        Ok(evaluator::evaluate_change(&refs, proposal))
    }

    // -----------------------------------------------------------------
    // Internals
    // -----------------------------------------------------------------

    fn exists(&self, ref_name: &str, normalized: &str) -> Result<bool, PolicyError> {
        match self.load_active(ref_name, normalized) {
            Ok(_) => Ok(true),
            Err(PolicyError::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn load_active(&self, ref_name: &str, normalized: &str) -> Result<Policy, PolicyError> {
        let path = paths::active(&self.prefix, normalized);
        let value = self.repo.get_json(ref_name, &path).map_err(|e| {
            if is_path_not_found(&e) {
                PolicyError::NotFound(normalized.to_string())
            } else {
                PolicyError::from(e)
            }
        })?;
        Ok(serde_json::from_value(value)?)
    }

    fn commit_opts(&self, category: &str, description: impl Into<String>) -> CommitOptions {
        CommitOptions::new(
            &self.agent_id,
            IntentCategory::Custom(category.to_string()),
            description,
        )
    }
}

fn is_path_not_found(e: &agentstategraph::RepoError) -> bool {
    matches!(e, agentstategraph::RepoError::Tree(_))
}
