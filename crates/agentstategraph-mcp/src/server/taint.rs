//! Taint, quarantine, and watch tool implementations.

use agentstategraph_policy::{ChangeProposal, Decision};

use super::{
    AgentStateGraphServer, CheckTaintParams, ListTaintsParams,
    PolicyEvaluateChangeWithTaintsParams, QuarantineApplyParams, TaintApplyParams,
    TaintRemoveParams, WatchApplyParams, WatchRemoveParams, parse_optional_rfc3339,
    parse_taint_effect, parse_taint_severity,
};

impl AgentStateGraphServer {
    pub(super) fn impl_taint(&self, p: TaintApplyParams) -> String {
        let effect = match parse_taint_effect(&p.effect) {
            Some(e) => e,
            None => {
                return serde_json::json!({ "error": format!("unknown effect: {}", p.effect) })
                    .to_string();
            }
        };
        let params = agentstategraph_taint::TaintParams {
            name: p.name,
            effect,
            reason: p.reason,
            severity: parse_taint_severity(p.severity.as_deref()),
            expires_at: parse_optional_rfc3339(p.expires.as_deref()),
            propagate: p.propagate.unwrap_or(true),
            metadata: agentstategraph_taint::TaintMetadata::new(),
            agent_id: p.agent_id,
        };
        match self.repo.taint(&p.r#ref, &p.path, params) {
            Ok(id) => serde_json::json!({ "ok": true, "id": id }).to_string(),
            Err(e) => serde_json::json!({ "error": e.to_string() }).to_string(),
        }
    }

    pub(super) fn impl_untaint(&self, p: TaintRemoveParams) -> String {
        let params = agentstategraph_taint::UntaintParams {
            reason: p.reason,
            proof: p.proof,
            agent_id: p.agent_id,
        };
        match self.repo.untaint(&p.r#ref, &p.path, &p.name, params) {
            Ok(()) => serde_json::json!({ "ok": true }).to_string(),
            Err(e) => serde_json::json!({ "error": e.to_string() }).to_string(),
        }
    }

    pub(super) fn impl_quarantine(&self, p: QuarantineApplyParams) -> String {
        let params = agentstategraph_taint::QuarantineParams {
            name: p.name,
            reason: p.reason,
            severity: parse_taint_severity(p.severity.as_deref()),
            authorized_agents: p.authorized_agents,
            expires_at: parse_optional_rfc3339(p.expires.as_deref()),
            propagate: p.propagate.unwrap_or(true),
            agent_id: p.agent_id,
        };
        match self.repo.quarantine(&p.r#ref, &p.path, params) {
            Ok(id) => serde_json::json!({ "ok": true, "id": id }).to_string(),
            Err(e) => serde_json::json!({ "error": e.to_string() }).to_string(),
        }
    }

    pub(super) fn impl_unquarantine(&self, p: TaintRemoveParams) -> String {
        let params = agentstategraph_taint::UntaintParams {
            reason: p.reason,
            proof: p.proof,
            agent_id: p.agent_id,
        };
        match self.repo.unquarantine(&p.r#ref, &p.path, &p.name, params) {
            Ok(()) => serde_json::json!({ "ok": true }).to_string(),
            Err(e) => serde_json::json!({ "error": e.to_string() }).to_string(),
        }
    }

    pub(super) fn impl_watch(&self, p: WatchApplyParams) -> String {
        let direction = match p.direction.as_deref().unwrap_or("above") {
            "below" => agentstategraph_taint::WatchDirection::Below,
            _ => agentstategraph_taint::WatchDirection::Above,
        };
        let params = agentstategraph_taint::WatchParams {
            name: p.name,
            reason: p.reason,
            metric: p.metric,
            threshold: p.threshold,
            direction,
            check_interval_secs: p.check_interval_secs,
            expires_at: parse_optional_rfc3339(p.expires.as_deref()),
            severity: parse_taint_severity(p.severity.as_deref()),
            propagate: p.propagate.unwrap_or(true),
            agent_id: p.agent_id,
        };
        match self.repo.watch_path(&p.r#ref, &p.path, params) {
            Ok(id) => serde_json::json!({ "ok": true, "id": id }).to_string(),
            Err(e) => serde_json::json!({ "error": e.to_string() }).to_string(),
        }
    }

    pub(super) fn impl_unwatch(&self, p: WatchRemoveParams) -> String {
        let params = agentstategraph_taint::UnwatchParams {
            reason: p.reason,
            agent_id: p.agent_id,
        };
        match self.repo.unwatch(&p.r#ref, &p.path, &p.name, params) {
            Ok(()) => serde_json::json!({ "ok": true }).to_string(),
            Err(e) => serde_json::json!({ "error": e.to_string() }).to_string(),
        }
    }

    pub(super) fn impl_list_taints(&self, p: ListTaintsParams) -> String {
        let kind = match p.kind.as_deref() {
            Some("taint") => Some(agentstategraph_taint::TaintKind::Taint),
            Some("quarantine") => Some(agentstategraph_taint::TaintKind::Quarantine),
            Some("watch") => Some(agentstategraph_taint::TaintKind::Watch),
            Some(other) => {
                return serde_json::json!({ "error": format!("unknown kind: {}", other) })
                    .to_string();
            }
            None => None,
        };
        match self
            .repo
            .list_taints(p.path.as_deref(), kind, p.include_expired.unwrap_or(false))
        {
            Ok(mut list) => {
                if let Some(effect) = p.effect.as_deref().and_then(parse_taint_effect) {
                    list.retain(|t| t.effect == effect);
                }
                serde_json::json!({ "ok": true, "taints": list }).to_string()
            }
            Err(e) => serde_json::json!({ "error": e.to_string() }).to_string(),
        }
    }

    pub(super) fn impl_check_taint(&self, p: CheckTaintParams) -> String {
        let agent_id = p.agent_id.as_deref().unwrap_or("");
        let confidence = p.confidence.unwrap_or(1.0);
        match self.repo.check_taint(&p.path, agent_id, confidence) {
            Ok(c) => serde_json::json!({ "ok": true, "check": c }).to_string(),
            Err(e) => serde_json::json!({ "error": e.to_string() }).to_string(),
        }
    }

    pub(super) fn impl_policy_evaluate_change_with_taints(
        &self,
        p: PolicyEvaluateChangeWithTaintsParams,
    ) -> String {
        let proposal: ChangeProposal = match serde_json::from_value(p.proposal.clone()) {
            Ok(p) => p,
            Err(e) => {
                return serde_json::json!({ "error": format!("invalid ChangeProposal: {e}") })
                    .to_string();
            }
        };
        let decision = match self.policies.evaluate_change_scoped(
            &p.r#ref,
            &proposal,
            p.tenant_filter.as_deref(),
        ) {
            Ok(d) => d,
            Err(e) => return serde_json::json!({ "error": e.to_string() }).to_string(),
        };
        let agent_id = p
            .agent_id
            .clone()
            .unwrap_or_else(|| proposal.agent_id.clone());
        let confidence = p.confidence.unwrap_or(1.0);
        let mut taint_status = Vec::new();
        let mut can_proceed = !matches!(decision, Decision::Deny { .. });
        for path in &p.affected_paths {
            match self.repo.check_taint(path, &agent_id, confidence) {
                Ok(c) => {
                    if !c.can_write {
                        can_proceed = false;
                    }
                    taint_status.push(serde_json::json!({
                        "path": path,
                        "check": c,
                    }));
                }
                Err(e) => {
                    return serde_json::json!({ "error": e.to_string() }).to_string();
                }
            }
        }
        serde_json::json!({
            "ok": true,
            "decision": decision,
            "taint_status": taint_status,
            "can_proceed": can_proceed,
        })
        .to_string()
    }
}
