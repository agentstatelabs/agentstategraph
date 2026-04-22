//! Repository-level taint / quarantine / watch methods (0.7.75 §4).
//!
//! Each method persists a [`Taint`] row via [`TaintStore`] and
//! writes an intent commit on the requested ref (the intent's
//! `IntentCategory` is Taint / Untaint / Quarantine / ...). The
//! storage row is patched with the commit id post-commit so the
//! two sides are cross-referenceable.
//!
//! The pre-commit hook ([`Repository::pre_commit_taint_check`]) is
//! threaded into `set` / `set_json` / `delete` / `merge` /
//! `commit_speculation` so writes to tainted / quarantined paths
//! see the decision from [`evaluate_access`].

use agentstategraph_core::IntentCategory;
use agentstategraph_taint::{
    QuarantineParams, Taint, TaintCheck, TaintEffect, TaintError, TaintKind, TaintMetadata,
    TaintParams, UnquarantineParams, UntaintParams, UnwatchParams, WatchParams, evaluate_access,
};
use chrono::Utc;

use crate::repo::{CommitOptions, RepoError, Repository};

impl Repository {
    // -----------------------------------------------------------------------
    // CRUD — create a taint / quarantine / watch + intent commit
    // -----------------------------------------------------------------------

    /// Apply a taint to `path`. Returns the new taint id.
    pub fn taint(
        &self,
        ref_name: &str,
        path: &str,
        params: TaintParams,
    ) -> Result<String, RepoError> {
        let id = uuid::Uuid::new_v4().to_string();
        let agent_id = params.agent_id.clone();
        let reason = params.reason.clone();
        let name = params.name.clone();
        let taint = Taint {
            id: id.clone(),
            path: path.to_string(),
            name: params.name,
            kind: TaintKind::Taint,
            effect: params.effect,
            severity: params.severity,
            reason: params.reason,
            agent_id: params.agent_id,
            commit_id: String::new(),
            created_at: Utc::now(),
            expires_at: params.expires_at,
            resolved_at: None,
            resolved_by: None,
            resolved_reason: None,
            resolved_proof: None,
            propagate: params.propagate,
            metadata: params.metadata,
        };
        self.taint_storage().create_taint(&taint)?;
        let commit_id = self.write_taint_intent(
            ref_name,
            IntentCategory::Taint,
            format!("taint {name} on {path}: {reason}"),
            &agent_id,
            Some(reason),
        )?;
        self.taint_storage()
            .set_taint_commit_id(&id, &format!("{commit_id}"))?;
        Ok(id)
    }

    /// Resolve a taint.
    pub fn untaint(
        &self,
        ref_name: &str,
        path: &str,
        taint_name: &str,
        params: UntaintParams,
    ) -> Result<(), RepoError> {
        self.resolve_kind(
            ref_name,
            path,
            taint_name,
            TaintKind::Taint,
            IntentCategory::Untaint,
            params,
        )
    }

    /// Apply a quarantine — restricts access to the
    /// `authorized_agents` list.
    pub fn quarantine(
        &self,
        ref_name: &str,
        path: &str,
        params: QuarantineParams,
    ) -> Result<String, RepoError> {
        let id = uuid::Uuid::new_v4().to_string();
        let agent_id = params.agent_id.clone();
        let reason = params.reason.clone();
        let name = params.name.clone();
        let mut metadata = TaintMetadata::new();
        metadata.insert(
            "authorized_agents",
            serde_json::json!(params.authorized_agents),
        );
        let taint = Taint {
            id: id.clone(),
            path: path.to_string(),
            name: params.name,
            kind: TaintKind::Quarantine,
            effect: TaintEffect::Block,
            severity: params.severity,
            reason: params.reason,
            agent_id: params.agent_id,
            commit_id: String::new(),
            created_at: Utc::now(),
            expires_at: params.expires_at,
            resolved_at: None,
            resolved_by: None,
            resolved_reason: None,
            resolved_proof: None,
            propagate: params.propagate,
            metadata,
        };
        self.taint_storage().create_taint(&taint)?;
        let commit_id = self.write_taint_intent(
            ref_name,
            IntentCategory::Quarantine,
            format!("quarantine {name} on {path}: {reason}"),
            &agent_id,
            Some(reason),
        )?;
        self.taint_storage()
            .set_taint_commit_id(&id, &format!("{commit_id}"))?;
        Ok(id)
    }

    /// Release a quarantine.
    pub fn unquarantine(
        &self,
        ref_name: &str,
        path: &str,
        taint_name: &str,
        params: UnquarantineParams,
    ) -> Result<(), RepoError> {
        self.resolve_kind(
            ref_name,
            path,
            taint_name,
            TaintKind::Quarantine,
            IntentCategory::Unquarantine,
            params,
        )
    }

    /// Apply an advisory watch.
    pub fn watch_path(
        &self,
        ref_name: &str,
        path: &str,
        params: WatchParams,
    ) -> Result<String, RepoError> {
        let id = uuid::Uuid::new_v4().to_string();
        let agent_id = params.agent_id.clone();
        let reason = params.reason.clone();
        let name = params.name.clone();
        let mut metadata = TaintMetadata::new();
        if let Some(m) = params.metric.as_ref() {
            metadata.insert("metric", serde_json::Value::String(m.clone()));
        }
        if let Some(t) = params.threshold {
            metadata.insert("threshold", serde_json::Value::from(t));
        }
        if let Ok(d) = serde_json::to_value(params.direction) {
            metadata.insert("direction", d);
        }
        if let Some(i) = params.check_interval_secs {
            metadata.insert("check_interval_secs", serde_json::Value::from(i));
        }
        let taint = Taint {
            id: id.clone(),
            path: path.to_string(),
            name: params.name,
            kind: TaintKind::Watch,
            effect: TaintEffect::Advisory,
            severity: params.severity,
            reason: params.reason,
            agent_id: params.agent_id,
            commit_id: String::new(),
            created_at: Utc::now(),
            expires_at: params.expires_at,
            resolved_at: None,
            resolved_by: None,
            resolved_reason: None,
            resolved_proof: None,
            propagate: params.propagate,
            metadata,
        };
        self.taint_storage().create_taint(&taint)?;
        let commit_id = self.write_taint_intent(
            ref_name,
            IntentCategory::Watch,
            format!("watch {name} on {path}: {reason}"),
            &agent_id,
            Some(reason),
        )?;
        self.taint_storage()
            .set_taint_commit_id(&id, &format!("{commit_id}"))?;
        Ok(id)
    }

    /// Remove a watch.
    pub fn unwatch(
        &self,
        ref_name: &str,
        path: &str,
        watch_name: &str,
        params: UnwatchParams,
    ) -> Result<(), RepoError> {
        let reason = params.reason.clone().unwrap_or_default();
        self.resolve_kind(
            ref_name,
            path,
            watch_name,
            TaintKind::Watch,
            IntentCategory::Unwatch,
            UntaintParams {
                reason,
                proof: None,
                agent_id: params.agent_id,
            },
        )
    }

    // -----------------------------------------------------------------------
    // Query helpers
    // -----------------------------------------------------------------------

    /// List taints / quarantines / watches, optionally filtered.
    pub fn list_taints(
        &self,
        path_prefix: Option<&str>,
        kind: Option<TaintKind>,
        include_resolved: bool,
    ) -> Result<Vec<Taint>, RepoError> {
        Ok(self
            .taint_storage()
            .list_taints(path_prefix, kind, include_resolved)?)
    }

    /// Aggregated check used by the pre-commit hook + policy
    /// integration. Safe to call without intent to write.
    pub fn check_taint(
        &self,
        path: &str,
        agent_id: &str,
        confidence: f64,
    ) -> Result<TaintCheck, RepoError> {
        let candidates = self.taint_storage().check_taint(path)?;
        Ok(evaluate_access(
            path,
            agent_id,
            confidence,
            &candidates,
            Utc::now(),
        ))
    }

    // -----------------------------------------------------------------------
    // Pre-commit hook
    // -----------------------------------------------------------------------

    /// Run the pre-commit taint check for `paths`. Returns the list
    /// of warnings that should be attached to the commit metadata;
    /// fails with `RepoError::Taint(...)` when the write is blocked.
    ///
    /// Called by `set` / `set_json` / `delete` / `merge` /
    /// `commit_speculation`.
    pub fn pre_commit_taint_check(
        &self,
        paths: &[&str],
        options: &CommitOptions,
    ) -> Result<Vec<String>, RepoError> {
        let agent_id = options.agent_id.as_str();
        let confidence = options.confidence.unwrap_or(1.0);
        let mut warnings = Vec::new();
        for p in paths {
            let check = self.check_taint(p, agent_id, confidence)?;
            if !check.can_write {
                if let Some(q) = check
                    .quarantines
                    .iter()
                    .find(|q| !q.authorized_agents().iter().any(|a| a == agent_id))
                {
                    return Err(taint_err_with_id(
                        TaintError::NotAuthorized {
                            path: (*p).to_string(),
                            agent_id: agent_id.to_string(),
                        },
                        q.id.clone(),
                    ));
                }
                if let Some(t) = check
                    .taints
                    .iter()
                    .find(|t| matches!(t.effect, TaintEffect::Block))
                {
                    return Err(taint_err_with_id(
                        TaintError::Blocked {
                            path: (*p).to_string(),
                            taint: t.name.clone(),
                            reason: t.reason.clone(),
                        },
                        t.id.clone(),
                    ));
                }
                if let Some(t) = check
                    .taints
                    .iter()
                    .find(|t| matches!(t.effect, TaintEffect::Review))
                {
                    return Err(taint_err_with_id(
                        TaintError::InsufficientConfidence {
                            path: (*p).to_string(),
                            taint: t.name.clone(),
                            required: check.required_confidence,
                            got: confidence,
                        },
                        t.id.clone(),
                    ));
                }
            }
            for t in check
                .taints
                .iter()
                .filter(|t| matches!(t.effect, TaintEffect::Warn | TaintEffect::Isolate))
            {
                warnings.push(format!(
                    "taint {name} on {tpath}: {reason}",
                    name = t.name,
                    tpath = t.path,
                    reason = t.reason,
                ));
            }
        }
        Ok(warnings)
    }

    // -----------------------------------------------------------------------
    // Internals
    // -----------------------------------------------------------------------

    fn resolve_kind(
        &self,
        ref_name: &str,
        path: &str,
        taint_name: &str,
        kind: TaintKind,
        intent: IntentCategory,
        params: UntaintParams,
    ) -> Result<(), RepoError> {
        let candidates = self
            .taint_storage()
            .list_taints(Some(path), Some(kind), false)?;
        let target = candidates
            .into_iter()
            .find(|t| t.path == path && t.name == taint_name)
            .ok_or_else(|| {
                RepoError::from(TaintError::NotFound(format!(
                    "{kind:?}:{path}:{taint_name}"
                )))
            })?;
        self.taint_storage().resolve_taint(
            &target.id,
            &params.agent_id,
            &params.reason,
            params.proof.as_deref(),
            Utc::now(),
        )?;
        self.write_taint_intent(
            ref_name,
            intent,
            format!("resolve {kind:?} {taint_name} on {path}"),
            &params.agent_id,
            Some(params.reason),
        )?;
        Ok(())
    }
}

fn taint_err_with_id(source: TaintError, id: String) -> RepoError {
    RepoError::Taint {
        source,
        taint_id: Some(id),
    }
}
