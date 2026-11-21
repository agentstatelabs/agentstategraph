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

use crate::context::CallContext;
use crate::repo::{CommitOptions, RepoError, Repository};
use crate::telemetry::{self, op_span};

impl Repository {
    // -----------------------------------------------------------------------
    // CRUD — create a taint / quarantine / watch + intent commit
    // -----------------------------------------------------------------------

    /// Apply a taint to `path`. Returns the new taint id. Its commits carry
    /// `params.agent_id`; [`Self::taint_with_context`] takes a whole caller.
    pub fn taint(
        &self,
        ref_name: &str,
        path: &str,
        params: TaintParams,
    ) -> Result<String, RepoError> {
        let caller = CallContext::new(params.agent_id.clone());
        self.taint_with_context(ref_name, path, params, &caller)
    }

    /// [`Self::taint`] for `caller` (DESIGN.md Plan F): the taint records the
    /// caller's agent in place of `params.agent_id`, and both of its commits,
    /// the intent commit on `ref_name` and the control-ref commit, carry the
    /// caller's agent, authority and device.
    pub fn taint_with_context(
        &self,
        ref_name: &str,
        path: &str,
        mut params: TaintParams,
        caller: &CallContext,
    ) -> Result<String, RepoError> {
        params.agent_id = caller.agent_id.clone();
        let id = uuid::Uuid::new_v4().to_string();
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
        let description = format!("taint {name} on {path}: {reason}");
        self.taint_through(&id, caller, IntentCategory::Taint, &description, || {
            self.taint_storage().create_taint(&taint)?;
            let commit_id = self.write_taint_intent(
                ref_name,
                IntentCategory::Taint,
                description.clone(),
                caller,
                Some(reason),
            )?;
            self.taint_storage()
                .set_taint_commit_id(&id, &format!("{commit_id}"))?;
            Ok(())
        })?;
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
        let caller = CallContext::new(params.agent_id.clone());
        self.untaint_with_context(ref_name, path, taint_name, params, &caller)
    }

    /// [`Self::untaint`] for `caller`: it resolves the taint as the caller's
    /// agent, in place of `params.agent_id`, and its commits carry the
    /// caller's agent, authority and device.
    pub fn untaint_with_context(
        &self,
        ref_name: &str,
        path: &str,
        taint_name: &str,
        params: UntaintParams,
        caller: &CallContext,
    ) -> Result<(), RepoError> {
        self.resolve_kind(ref_name, path, taint_name, TaintKind::Taint, params, caller)
    }

    /// Apply a quarantine — restricts access to the
    /// `authorized_agents` list.
    pub fn quarantine(
        &self,
        ref_name: &str,
        path: &str,
        params: QuarantineParams,
    ) -> Result<String, RepoError> {
        let caller = CallContext::new(params.agent_id.clone());
        self.quarantine_with_context(ref_name, path, params, &caller)
    }

    /// [`Self::quarantine`] for `caller`: the quarantine records the caller's
    /// agent in place of `params.agent_id`, and its commits carry the
    /// caller's agent, authority and device.
    pub fn quarantine_with_context(
        &self,
        ref_name: &str,
        path: &str,
        mut params: QuarantineParams,
        caller: &CallContext,
    ) -> Result<String, RepoError> {
        params.agent_id = caller.agent_id.clone();
        let id = uuid::Uuid::new_v4().to_string();
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
        let description = format!("quarantine {name} on {path}: {reason}");
        self.taint_through(
            &id,
            caller,
            IntentCategory::Quarantine,
            &description,
            || {
                self.taint_storage().create_taint(&taint)?;
                let commit_id = self.write_taint_intent(
                    ref_name,
                    IntentCategory::Quarantine,
                    description.clone(),
                    caller,
                    Some(reason),
                )?;
                self.taint_storage()
                    .set_taint_commit_id(&id, &format!("{commit_id}"))?;
                Ok(())
            },
        )?;
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
        let caller = CallContext::new(params.agent_id.clone());
        self.unquarantine_with_context(ref_name, path, taint_name, params, &caller)
    }

    /// [`Self::unquarantine`] for `caller`, as [`Self::untaint_with_context`]
    /// is for a taint.
    pub fn unquarantine_with_context(
        &self,
        ref_name: &str,
        path: &str,
        taint_name: &str,
        params: UnquarantineParams,
        caller: &CallContext,
    ) -> Result<(), RepoError> {
        self.resolve_kind(
            ref_name,
            path,
            taint_name,
            TaintKind::Quarantine,
            params,
            caller,
        )
    }

    /// Apply an advisory watch.
    pub fn watch_path(
        &self,
        ref_name: &str,
        path: &str,
        params: WatchParams,
    ) -> Result<String, RepoError> {
        let caller = CallContext::new(params.agent_id.clone());
        self.watch_path_with_context(ref_name, path, params, &caller)
    }

    /// [`Self::watch_path`] for `caller`: the watch records the caller's
    /// agent in place of `params.agent_id`, and its commits carry the
    /// caller's agent, authority and device.
    pub fn watch_path_with_context(
        &self,
        ref_name: &str,
        path: &str,
        mut params: WatchParams,
        caller: &CallContext,
    ) -> Result<String, RepoError> {
        params.agent_id = caller.agent_id.clone();
        let id = uuid::Uuid::new_v4().to_string();
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
        let description = format!("watch {name} on {path}: {reason}");
        self.taint_through(&id, caller, IntentCategory::Watch, &description, || {
            self.taint_storage().create_taint(&taint)?;
            let commit_id = self.write_taint_intent(
                ref_name,
                IntentCategory::Watch,
                description.clone(),
                caller,
                Some(reason),
            )?;
            self.taint_storage()
                .set_taint_commit_id(&id, &format!("{commit_id}"))?;
            Ok(())
        })?;
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
        let caller = CallContext::new(params.agent_id.clone());
        self.unwatch_with_context(ref_name, path, watch_name, params, &caller)
    }

    /// [`Self::unwatch`] for `caller`, as [`Self::untaint_with_context`] is
    /// for a taint.
    pub fn unwatch_with_context(
        &self,
        ref_name: &str,
        path: &str,
        watch_name: &str,
        params: UnwatchParams,
        caller: &CallContext,
    ) -> Result<(), RepoError> {
        let reason = params.reason.clone().unwrap_or_default();
        self.resolve_kind(
            ref_name,
            path,
            watch_name,
            TaintKind::Watch,
            UntaintParams {
                reason,
                proof: None,
                agent_id: params.agent_id,
            },
            caller,
        )
    }

    // -----------------------------------------------------------------------
    // Query helpers
    // -----------------------------------------------------------------------

    /// Fetch a taint / quarantine / watch record by its storage id.
    pub fn get_taint(&self, id: &str) -> Result<Option<Taint>, RepoError> {
        Ok(self.taint_storage().get_taint(id)?)
    }

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
        telemetry::run(op_span!(INFO, "asg.taint.check"), || {
            let result = self.pre_commit_taint_check_inner(paths, options);
            if let Err(RepoError::Taint { source, taint_id }) = &result {
                telemetry::taint_refused(source, taint_id.as_deref());
            }
            result
        })
    }

    fn pre_commit_taint_check_inner(
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

    /// Inspect active watches on `path` and auto-escalate any whose
    /// numeric threshold was just crossed by the commit that wrote
    /// `new_value`. Runs at most once per `(watch.id, commit)` pair —
    /// if an auto-taint already exists for the same
    /// `watch-threshold-exceeded-<watch.name>` name and is
    /// unresolved, no new taint is created (idempotent).
    ///
    /// Called by `set_json` after the commit is persisted (§5).
    pub fn auto_escalate_watches(
        &self,
        ref_name: &str,
        path: &str,
        new_value: &serde_json::Value,
    ) -> Result<Vec<String>, RepoError> {
        let candidates = self.taint_storage().check_taint(path)?;
        let mut created = Vec::new();
        for w in candidates.iter().filter(|t| t.kind == TaintKind::Watch) {
            let metric = w.metadata.get("metric").and_then(|v| v.as_str());
            let threshold = w.metadata.get("threshold").and_then(|v| v.as_f64());
            let direction = w
                .metadata
                .get("direction")
                .and_then(|v| v.as_str())
                .unwrap_or("above");
            let (Some(metric), Some(threshold)) = (metric, threshold) else {
                continue;
            };
            let Some(current) = extract_metric(new_value, metric) else {
                continue;
            };
            let crossed = match direction {
                "below" => current < threshold,
                _ => current > threshold,
            };
            if !crossed {
                continue;
            }
            let auto_name = format!("watch-threshold-exceeded-{}", w.name);
            // Idempotence: skip if an unresolved auto-taint with
            // this name already exists on this path.
            let existing =
                self.taint_storage()
                    .list_taints(Some(&w.path), Some(TaintKind::Taint), false)?;
            if existing.iter().any(|t| t.name == auto_name) {
                continue;
            }
            let reason = format!(
                "Watch threshold exceeded: {metric} = {current} {cmp} {threshold}",
                cmp = if direction == "below" { "<" } else { ">" }
            );
            let mut metadata = TaintMetadata::new();
            metadata.insert("auto_escalated", serde_json::Value::Bool(true));
            metadata.insert("source_watch_id", serde_json::Value::String(w.id.clone()));
            metadata.insert("metric", serde_json::Value::String(metric.to_string()));
            metadata.insert("threshold", serde_json::Value::from(threshold));
            metadata.insert("observed", serde_json::Value::from(current));
            let id = self.taint(
                ref_name,
                &w.path,
                TaintParams {
                    name: auto_name,
                    effect: TaintEffect::Warn,
                    reason,
                    severity: w.severity,
                    expires_at: None,
                    propagate: w.propagate,
                    metadata,
                    agent_id: format!("watch/{}", w.id),
                },
            )?;
            created.push(id);
        }
        Ok(created)
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
        params: UntaintParams,
        caller: &CallContext,
    ) -> Result<(), RepoError> {
        let intent = match kind {
            TaintKind::Taint => IntentCategory::Untaint,
            TaintKind::Quarantine => IntentCategory::Unquarantine,
            TaintKind::Watch => IntentCategory::Unwatch,
        };
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
        let description = format!("resolve {kind:?} {taint_name} on {path}");
        self.taint_through(&target.id, caller, intent.clone(), &description, || {
            self.taint_storage().resolve_taint(
                &target.id,
                &caller.agent_id,
                &params.reason,
                params.proof.as_deref(),
                Utc::now(),
            )?;
            self.write_taint_intent(
                ref_name,
                intent,
                description.clone(),
                caller,
                Some(params.reason),
            )?;
            Ok(())
        })
    }

    /// Run a taint table write, then commit the taint to the control ref
    /// (DESIGN.md Plan G t-001), with the same provenance as its intent
    /// commit: `caller`'s.
    fn taint_through(
        &self,
        id: &str,
        caller: &CallContext,
        category: IntentCategory,
        description: &str,
        table: impl FnOnce() -> Result<(), RepoError>,
    ) -> Result<(), RepoError> {
        self.write_through(
            crate::control_repo::Stage::Record(crate::control_repo::Kind::Taint, id),
            caller.commit_options(category, description),
            table,
        )
    }
}

fn taint_err_with_id(source: TaintError, id: String) -> RepoError {
    RepoError::Taint {
        source,
        taint_id: Some(id),
    }
}

/// Extract a numeric value for `metric` from a JSON value. Supports:
/// - a top-level number (when `metric == ""` or the value IS the
///   number being watched);
/// - a flat object with a numeric field keyed by `metric`;
/// - stringified numbers (e.g. `"82"`) commonly produced by shell
///   scrapers.
fn extract_metric(value: &serde_json::Value, metric: &str) -> Option<f64> {
    if let Some(n) = value.as_f64() {
        return Some(n);
    }
    if let Some(n) = value.as_str().and_then(|s| s.parse::<f64>().ok()) {
        return Some(n);
    }
    if let Some(v) = value.as_object().and_then(|o| o.get(metric)) {
        if let Some(n) = v.as_f64() {
            return Some(n);
        }
        if let Some(n) = v.as_str().and_then(|s| s.parse::<f64>().ok()) {
            return Some(n);
        }
    }
    None
}
