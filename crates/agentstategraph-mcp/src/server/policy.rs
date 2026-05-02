//! Policy tool implementations.

use agentstategraph_policy::{ChangeProposal, Policy, PolicySignature, Situation};
use agentstategraph_policy_sign::canonicalize;

use super::{
    AgentStateGraphServer, PolicyCheckTokensParams, PolicyEvaluateChangeParams,
    PolicyEvaluateParams, PolicyHistoryParams, PolicyListParams, PolicyProposeParams,
    PolicyRatifyParams, PolicyShowParams, PolicySignParams, PolicySupersedeParams,
    PolicyVerifyParams, render_decision_with_fail_safe,
};

impl AgentStateGraphServer {
    pub(super) fn impl_policy_propose(&self, p: PolicyProposeParams) -> String {
        let policy: Policy = match serde_json::from_value(p.policy) {
            Ok(p) => p,
            Err(e) => return format!("Error: invalid Policy JSON: {}", e),
        };
        match self.policies.propose(&p.r#ref, policy) {
            Ok(handle) => format!("Proposed {}", handle),
            Err(e) => format!("Error: {}", e),
        }
    }

    pub(super) fn impl_policy_ratify(&self, p: PolicyRatifyParams) -> String {
        match self
            .policies
            .ratify(&p.r#ref, &p.path, &p.ratifier, &p.reasoning)
        {
            Ok(()) => format!("Ratified {} by {}", p.path, p.ratifier),
            Err(e) => format!("Error: {}", e),
        }
    }

    pub(super) fn impl_policy_supersede(&self, p: PolicySupersedeParams) -> String {
        let new_policy: Policy = match serde_json::from_value(p.new_policy) {
            Ok(p) => p,
            Err(e) => return format!("Error: invalid Policy JSON: {}", e),
        };
        match self.policies.supersede(&p.r#ref, &p.old_path, new_policy) {
            Ok(handle) => format!("Superseded → {}", handle),
            Err(e) => format!("Error: {}", e),
        }
    }

    pub(super) fn impl_policy_list(&self, p: PolicyListParams) -> String {
        let status = p.status.as_deref().unwrap_or("active").to_lowercase();
        let tenant = p.tenant_filter.as_deref();
        let result = match status.as_str() {
            "proposed" => self
                .policies
                .list_scoped(&p.r#ref, p.prefix.as_deref(), tenant)
                .map(|ps| ps.into_iter().filter(|p| !p.is_ratified()).collect()),
            "all" => self
                .policies
                .list_scoped(&p.r#ref, p.prefix.as_deref(), tenant),
            _ => self
                .policies
                .active_scoped(&p.r#ref, p.prefix.as_deref(), tenant),
        };
        match result {
            Ok(policies) => serde_json::to_string_pretty(&policies).unwrap_or_default(),
            Err(e) => format!("Error: {}", e),
        }
    }

    pub(super) fn impl_policy_show(&self, p: PolicyShowParams) -> String {
        match self.policies.get(&p.r#ref, &p.path, p.version) {
            Ok(policy) => serde_json::to_string_pretty(&policy).unwrap_or_default(),
            Err(e) => format!("Error: {}", e),
        }
    }

    pub(super) fn impl_policy_history(&self, p: PolicyHistoryParams) -> String {
        match self.policies.history(&p.r#ref, &p.path) {
            Ok(chain) => serde_json::to_string_pretty(&chain).unwrap_or_default(),
            Err(e) => format!("Error: {}", e),
        }
    }

    pub(super) fn impl_policy_evaluate(&self, p: PolicyEvaluateParams) -> String {
        let situation = Situation(p.situation);
        match self.policies.evaluate_scoped(
            &p.r#ref,
            &situation,
            &p.action,
            &p.agent_id,
            p.tenant_filter.as_deref(),
        ) {
            Ok(decision) => render_decision_with_fail_safe(&decision, &self.policy_fail_safe),
            Err(e) => format!("Error: {}", e),
        }
    }

    pub(super) fn impl_policy_evaluate_change(&self, p: PolicyEvaluateChangeParams) -> String {
        let proposal: ChangeProposal = match serde_json::from_value(p.proposal) {
            Ok(p) => p,
            Err(e) => return format!("Error: invalid ChangeProposal JSON: {}", e),
        };
        match self
            .policies
            .evaluate_change_scoped(&p.r#ref, &proposal, p.tenant_filter.as_deref())
        {
            Ok(decision) => render_decision_with_fail_safe(&decision, &self.policy_fail_safe),
            Err(e) => format!("Error: {}", e),
        }
    }

    pub(super) fn impl_policy_check_tokens(&self, p: PolicyCheckTokensParams) -> String {
        match self.policies.active(&p.r#ref, None) {
            Ok(policies) => {
                let token_set: std::collections::HashSet<&str> =
                    p.tokens.iter().map(String::as_str).collect();
                let matches: Vec<serde_json::Value> = policies
                    .iter()
                    .filter(|policy| {
                        policy
                            .triggers
                            .iter()
                            .any(|t| token_set.contains(t.as_str()))
                    })
                    .map(|policy| {
                        let hit: Vec<&String> = policy
                            .triggers
                            .iter()
                            .filter(|t| token_set.contains(t.as_str()))
                            .collect();
                        serde_json::json!({
                            "policy": policy.handle(),
                            "matched_triggers": hit,
                            "severity": policy.severity,
                            "required_fields": policy.required_fields,
                        })
                    })
                    .collect();
                serde_json::to_string_pretty(&matches).unwrap_or_default()
            }
            Err(e) => format!("Error: {}", e),
        }
    }

    pub(super) fn impl_policy_sign(&self, p: PolicySignParams) -> String {
        let Some(signer) = self.signer.as_ref() else {
            return serde_json::json!({ "error": "no signer registered" }).to_string();
        };
        let policy = match self.policies.get(&p.r#ref, &p.path, None) {
            Ok(pol) => pol,
            Err(e) => return serde_json::json!({ "error": e.to_string() }).to_string(),
        };
        let canonical = match canonicalize(&policy) {
            Ok(c) => c,
            Err(e) => {
                return serde_json::json!({ "error": format!("canonicalize: {}", e) }).to_string();
            }
        };
        let (key_id, sig_bytes) = match signer.sign(&canonical) {
            Ok(pair) => pair,
            Err(e) => return serde_json::json!({ "error": format!("sign: {}", e) }).to_string(),
        };
        // `signer_key_id` param is advisory — `Ed25519Signer` returns its
        // configured key_id. We surface the one the signer actually used.
        let _requested = p.signer_key_id;
        let signature = PolicySignature::Ed25519 {
            signer_key_id: key_id,
            signature_hex: hex::encode(&sig_bytes),
        };
        if let Err(e) = self
            .policies
            .set_signature(&p.r#ref, &p.path, signature.clone())
        {
            return serde_json::json!({ "error": e.to_string() }).to_string();
        }
        serde_json::json!({
            "ok": true,
            "signature": signature,
        })
        .to_string()
    }

    pub(super) fn impl_policy_verify(&self, p: PolicyVerifyParams) -> String {
        let Some(verifier) = self.verifier.as_ref() else {
            return serde_json::json!({
                "valid": serde_json::Value::Null,
                "reason": "no verifier registered",
            })
            .to_string();
        };
        let policy = match self.policies.get(&p.r#ref, &p.path, None) {
            Ok(pol) => pol,
            Err(e) => return serde_json::json!({ "error": e.to_string() }).to_string(),
        };
        match verifier.verify_policy(&policy) {
            Ok(()) => serde_json::json!({ "valid": true }).to_string(),
            Err(e) => serde_json::json!({
                "valid": false,
                "reason": e.to_string(),
            })
            .to_string(),
        }
    }
}
