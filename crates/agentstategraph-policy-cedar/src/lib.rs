//! Cedar runner for AgentStateGraph policies (0.7.5 §4b, stub).
//!
//! # Status: stub
//!
//! Cedar's CLI (`cedar authorize`) takes a fairly involved set of
//! inputs — policies file, entities file, and a request-json file
//! whose schema depends on the user's entity model. Wiring those
//! together correctly needs design choices that are out of scope for
//! §4b of the 0.7.5-beta.1 plan (which is about landing the three
//! runner crates as pluggable sibling libraries, not about shipping a
//! feature-complete Cedar adapter).
//!
//! This crate therefore ships a **stub** [`CedarEvaluator`] that:
//!
//! - Accepts [`EvaluatorSource::Inline`] / [`EvaluatorSource::FilePath`]
//!   (so type-check and registration still succeed), and rejects
//!   [`EvaluatorSource::CommitRef`] with
//!   [`ExternalError::SourceResolution`] (matching the Wasm and Rego
//!   runners).
//! - Always returns [`ExternalError::Execution`] with a descriptive
//!   message pointing at the Cedar schema decisions that still need
//!   to be made, so operators wiring this up in anger get a clear
//!   signal instead of silent miscompiles.
//!
//! The planned mapping (for the follow-up commit that lands the real
//! impl) is:
//!
//! - `principal` = `Agent::"<agent_id>"`
//! - `action`    = `Action::"<action>"`
//! - `resource`  = `Situation::"default"`
//! - `context`   = `{ "situation": <situation-map> }`
//!
//! Invocation sketch:
//!
//! ```text
//! cedar authorize \
//!   --policies  <policies.cedar> \
//!   --entities  <entities.json> \
//!   --request-json <request.json>
//! ```
//!
//! The decision is read from the `decision` field of the JSON output
//! and mapped: `Allow` → [`Decision::Allow`], `Deny` →
//! [`Decision::Deny`], everything else →
//! [`Decision::NoPolicyMatch`]. `matched_policy` is derived from the
//! `determining_policies` field.

use agentstategraph_policy::external::{ExternalError, ExternalEvaluator};
use agentstategraph_policy::selector::Situation;
use agentstategraph_policy::types::{Decision, EvaluatorSource};

/// Stub Cedar runner. See the module-level docs for the planned
/// real integration.
pub struct CedarEvaluator {
    /// Path (or bare name) of the `cedar` binary. Defaults to
    /// `"cedar"` (resolved on `$PATH`). Kept here for API
    /// compatibility with the forthcoming real implementation.
    #[allow(dead_code)]
    cedar_path: String,
}

impl CedarEvaluator {
    /// Construct with the default `"cedar"` binary.
    pub fn new() -> Self {
        Self {
            cedar_path: "cedar".into(),
        }
    }

    /// Override the Cedar binary path.
    pub fn with_cedar_path(cedar_path: impl Into<String>) -> Self {
        Self {
            cedar_path: cedar_path.into(),
        }
    }
}

impl Default for CedarEvaluator {
    fn default() -> Self {
        Self::new()
    }
}

impl ExternalEvaluator for CedarEvaluator {
    fn kind(&self) -> &'static str {
        "cedar"
    }

    fn evaluate(
        &self,
        source: &EvaluatorSource,
        _situation: &Situation,
        _action: &str,
        _agent_id: &str,
    ) -> Result<Decision, ExternalError> {
        // Preserve the CommitRef rejection contract shared with the
        // Wasm and Rego runners.
        if let EvaluatorSource::CommitRef { .. } = source {
            return Err(ExternalError::SourceResolution(
                "commit_ref not supported by CedarEvaluator".into(),
            ));
        }

        Err(ExternalError::Execution(
            "CedarEvaluator is a stub: full Cedar integration lands as a \
             follow-up commit. The expected mapping is principal = \
             Agent::\"<agent_id>\", action = Action::\"<action>\", \
             resource = Situation::\"default\", context = {situation: \
             <map>}. See module docs."
                .into(),
        ))
    }
}
