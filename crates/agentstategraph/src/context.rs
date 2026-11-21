//! `CallContext`: who a host says a call is from.
//!
//! ASG doesn't authenticate; the host does (DESIGN.md Plan F). A host that
//! has verified who is calling puts it in a `CallContext` and builds each
//! commit's [`CommitOptions`] from it, so the agent, the authorizing
//! principal, the delegation chain and the device land on the commit the
//! same way whichever write path made it.
//!
//! This is the provenance slice of Plan F t-001, shipped first. It carries
//! no namespace, session, epoch or request id yet, and there is no
//! `ScopedRepo` or `Authorizer`: those come with the rest of t-001 and
//! t-002, which grow this type rather than replace it. It is
//! `#[non_exhaustive]` so they can add fields without breaking callers;
//! build it with [`CallContext::new`] and the `with_*` methods.

use agentstategraph_core::{
    AgentId, Authority, DelegationLink, DeviceId, IntentCategory, Principal,
};

use crate::repo::CommitOptions;

/// The verified identity behind a call, as provenance for the commits it
/// makes.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct CallContext {
    /// Who is acting: the agent (or person) making the change.
    pub agent_id: AgentId,
    /// Who authorized it. `None` means the actor authorized it themselves,
    /// as [`CommitOptions::new`] assumes: the principal is `agent_id`.
    pub principal: Option<Principal>,
    /// The delegation chain from the principal to the agent, recorded on
    /// each commit's [`Authority::delegation_chain`]. Empty for none.
    pub delegation: Vec<DelegationLink>,
    /// The device the call came from, when the host knows it. Stored on
    /// each commit as [`agentstategraph_core::Commit::device`].
    pub device: Option<DeviceId>,
}

impl CallContext {
    /// A context for `agent_id`, authorizing itself, with no delegation and
    /// no device.
    pub fn new(agent_id: impl Into<AgentId>) -> Self {
        Self {
            agent_id: agent_id.into(),
            principal: None,
            delegation: Vec::new(),
            device: None,
        }
    }

    /// Set who authorized the call, when it isn't the agent itself.
    pub fn with_principal(mut self, principal: impl Into<Principal>) -> Self {
        self.principal = Some(principal.into());
        self
    }

    /// Set the delegation chain from the principal to the agent.
    pub fn with_delegation(mut self, delegation: Vec<DelegationLink>) -> Self {
        self.delegation = delegation;
        self
    }

    /// Set the device the call came from.
    pub fn with_device(mut self, device: impl Into<DeviceId>) -> Self {
        self.device = Some(device.into());
        self
    }

    /// The principal that authorized the call: the one set with
    /// [`Self::with_principal`], else the agent.
    pub fn principal(&self) -> &str {
        self.principal.as_deref().unwrap_or(&self.agent_id)
    }

    /// The authority a commit made under this context records: the
    /// principal, wildcard scope and the delegation chain.
    pub fn authority(&self) -> Authority {
        let mut authority = Authority::simple(self.principal());
        authority.delegation_chain = self.delegation.clone();
        authority
    }

    /// Commit options carrying this context's agent, authority and device,
    /// for a change of `category` described by `description`. Add
    /// reasoning, confidence, tags or tool calls with the usual
    /// [`CommitOptions`] builders.
    pub fn commit_options(
        &self,
        category: IntentCategory,
        description: impl Into<String>,
    ) -> CommitOptions {
        let mut options = CommitOptions::new(self.agent_id.clone(), category, description)
            .with_authority(self.authority());
        options.device = self.device.clone();
        options
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentstategraph_core::AuthScope;
    use chrono::Utc;

    #[test]
    fn a_bare_context_commits_like_commit_options_new() {
        let ctx = CallContext::new("agent/a");
        let from_ctx = ctx.commit_options(IntentCategory::Refine, "x");
        let plain = CommitOptions::new("agent/a", IntentCategory::Refine, "x");
        assert_eq!(from_ctx.agent_id, plain.agent_id);
        assert_eq!(from_ctx.authority.principal, plain.authority.principal);
        assert_eq!(from_ctx.authority.scope, plain.authority.scope);
        assert!(from_ctx.authority.delegation_chain.is_empty());
        assert_eq!(from_ctx.device, None);
        assert_eq!(from_ctx.intent.description, "x");
    }

    #[test]
    fn a_full_context_fills_principal_delegation_and_device() {
        let link = DelegationLink {
            from: "human/alice".into(),
            to: "agent/a".into(),
            scope: AuthScope::Branch("main".into()),
            granted_at: Utc::now(),
            expires: None,
        };
        let ctx = CallContext::new("agent/a")
            .with_principal("human/alice")
            .with_delegation(vec![link.clone()])
            .with_device("0123456789abcdef0123456789abcdef");
        assert_eq!(ctx.principal(), "human/alice");
        let options = ctx.commit_options(IntentCategory::Fix, "patch");
        assert_eq!(options.agent_id, "agent/a");
        assert_eq!(options.authority.principal, "human/alice");
        assert_eq!(options.authority.delegation_chain, vec![link]);
        assert_eq!(
            options.device.as_deref(),
            Some("0123456789abcdef0123456789abcdef")
        );
    }
}
