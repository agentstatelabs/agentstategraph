//! OPA/Rego runner for AgentStateGraph policies (0.7.5 §4b).
//!
//! [`RegoEvaluator`] shells out to the `opa` binary on `$PATH` (or an
//! explicit override via [`RegoEvaluator::with_opa_path`]). The Rego
//! source is written to a temp file, the evaluation input is streamed
//! in on stdin, and the decision is pulled from
//! `data.policy.decision`.
//!
//! # Policy contract
//!
//! The Rego module MUST declare `package policy` and define a
//! `decision` rule that evaluates to a JSON object matching
//! [`Decision`]'s serde representation, e.g.:
//!
//! ```rego
//! package policy
//!
//! decision := {
//!   "kind": "allow",
//!   "matched_policy": "infra/my-policy@1",
//!   "preconditions": []
//! } {
//!   input.action == "deploy"
//! }
//! ```
//!
//! # Input shape
//!
//! ```json
//! {
//!   "situation": { "<key>": "<value>", ... },
//!   "action": "<action>",
//!   "agent_id": "<id>"
//! }
//! ```
//!
//! # Source variants
//!
//! - [`EvaluatorSource::Inline`] — body is Rego source text.
//! - [`EvaluatorSource::FilePath`] — Rego source is read from disk.
//! - [`EvaluatorSource::CommitRef`] — unsupported; returns
//!   [`ExternalError::SourceResolution`].

use std::io::Write;
use std::process::{Command, Stdio};

use agentstategraph_policy::external::{ExternalError, ExternalEvaluator};
use agentstategraph_policy::selector::Situation;
use agentstategraph_policy::types::{Decision, EvaluatorSource};

/// Shells out to `opa eval`.
pub struct RegoEvaluator {
    /// Path (or bare name) of the OPA binary. Defaults to `"opa"`
    /// (resolved against `$PATH`).
    opa_path: String,
}

impl RegoEvaluator {
    /// Construct with the default `"opa"` binary (resolved on `$PATH`).
    pub fn new() -> Self {
        Self {
            opa_path: "opa".into(),
        }
    }

    /// Override the OPA binary path. Useful for tests and for servers
    /// that ship a vendored OPA next to their own binary.
    pub fn with_opa_path(opa_path: impl Into<String>) -> Self {
        Self {
            opa_path: opa_path.into(),
        }
    }
}

impl Default for RegoEvaluator {
    fn default() -> Self {
        Self::new()
    }
}

impl ExternalEvaluator for RegoEvaluator {
    fn kind(&self) -> &'static str {
        "rego"
    }

    fn evaluate(
        &self,
        source: &EvaluatorSource,
        situation: &Situation,
        action: &str,
        agent_id: &str,
    ) -> Result<Decision, ExternalError> {
        // 1. Resolve source -> Rego text.
        let rego = match source {
            EvaluatorSource::Inline { body } => body.clone(),
            EvaluatorSource::FilePath { path } => std::fs::read_to_string(path)
                .map_err(|e| ExternalError::SourceResolution(e.to_string()))?,
            EvaluatorSource::CommitRef { .. } => {
                return Err(ExternalError::SourceResolution(
                    "commit_ref not supported by RegoEvaluator".into(),
                ));
            }
        };

        // 2. Write to a temp file; OPA expects `--data <file>`.
        let tmp = tempfile::Builder::new()
            .suffix(".rego")
            .tempfile()
            .map_err(|e| ExternalError::Execution(format!("tempfile: {e}")))?;
        std::fs::write(tmp.path(), rego.as_bytes())
            .map_err(|e| ExternalError::Execution(format!("write rego: {e}")))?;

        // 3. Build the input envelope.
        let input = serde_json::json!({
            "situation": &situation.0,
            "action": action,
            "agent_id": agent_id,
        });
        let input_bytes = serde_json::to_vec(&input)
            .map_err(|e| ExternalError::Execution(format!("input serialize: {e}")))?;

        // 4. Spawn `opa eval`.
        let mut child = Command::new(&self.opa_path)
            .arg("eval")
            .arg("--data")
            .arg(tmp.path())
            .arg("--stdin-input")
            .arg("--format=json")
            .arg("data.policy.decision")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                ExternalError::Execution(format!(
                    "failed to invoke {}: {e} (is OPA on $PATH?)",
                    self.opa_path
                ))
            })?;

        // 5. Write input and wait.
        {
            let stdin = child
                .stdin
                .as_mut()
                .ok_or_else(|| ExternalError::Execution("failed to open opa stdin".into()))?;
            stdin
                .write_all(&input_bytes)
                .map_err(|e| ExternalError::Execution(format!("write stdin: {e}")))?;
        }
        let output = child
            .wait_with_output()
            .map_err(|e| ExternalError::Execution(format!("wait opa: {e}")))?;

        if !output.status.success() {
            return Err(ExternalError::Execution(format!(
                "opa eval exited {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        // 6. Parse the OPA envelope.
        // {"result": [{"expressions": [{"value": <Decision>, ...}]}]}
        let envelope: serde_json::Value = serde_json::from_slice(&output.stdout)
            .map_err(|e| ExternalError::Execution(format!("parse opa output: {e}")))?;
        let decision_value = envelope
            .pointer("/result/0/expressions/0/value")
            .cloned()
            .ok_or_else(|| {
                ExternalError::Execution("opa output missing /result/0/expressions/0/value".into())
            })?;
        let decision: Decision = serde_json::from_value(decision_value)
            .map_err(|e| ExternalError::Execution(format!("parse decision: {e}")))?;
        Ok(decision)
    }
}
