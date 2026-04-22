//! Cedar runner for AgentStateGraph policies (0.7.5 §4b).
//!
//! [`CedarEvaluator`] shells out to the `cedar` binary on `$PATH` (or
//! an explicit override via [`CedarEvaluator::new_with_path`]). The
//! Cedar source is written to a temp file, an `entities.json` and a
//! `request.json` are synthesized per-call, and the decision is pulled
//! from the `decision` field of `cedar authorize`'s JSON output.
//!
//! # Entity / request model
//!
//! A single `Agent` entity is synthesized per call:
//!
//! ```json
//! { "uid": {"type": "Agent", "id": "<agent_id>"}, "attrs": {}, "parents": [] }
//! ```
//!
//! The request is:
//!
//! - `principal` = `Agent::"<agent_id>"`
//! - `action`    = `Action::"<action>"`
//! - `resource`  = `Situation::"default"`
//! - `context`   = `{ "situation": <situation-map> }`
//!
//! Invocation:
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
//! [`Decision::Deny`], everything else → [`Decision::NoPolicyMatch`].
//! `matched_policy` is the first entry in `determining_policies`, or
//! the literal `"cedar"` if absent.
//!
//! # Source variants
//!
//! - [`EvaluatorSource::Inline`] — body is Cedar policy source text.
//! - [`EvaluatorSource::FilePath`] — Cedar source path is passed
//!   through to `cedar authorize --policies`.
//! - [`EvaluatorSource::CommitRef`] — unsupported here; resolve via
//!   [`PolicyStore`] before dispatch. Returns
//!   [`ExternalError::SourceResolution`].

use std::io::ErrorKind;
use std::process::Command;

use agentstategraph_policy::external::{ExternalError, ExternalEvaluator};
use agentstategraph_policy::selector::Situation;
use agentstategraph_policy::types::{Decision, EvaluatorSource};

/// Shells out to `cedar authorize`.
pub struct CedarEvaluator {
    /// Path (or bare name) of the `cedar` binary. Defaults to
    /// `"cedar"` (resolved against `$PATH`).
    cedar_path: String,
}

impl CedarEvaluator {
    /// Construct with the default `"cedar"` binary (resolved on `$PATH`).
    pub fn new() -> Self {
        Self {
            cedar_path: "cedar".into(),
        }
    }

    /// Override the Cedar binary path. Useful for tests and for
    /// servers that ship a vendored `cedar` next to their own binary.
    pub fn new_with_path(cedar_path: impl Into<String>) -> Self {
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
        situation: &Situation,
        action: &str,
        agent_id: &str,
    ) -> Result<Decision, ExternalError> {
        // 1. Resolve the policies-file path. For Inline we stage to a
        //    temp file; for FilePath we pass through unchanged; for
        //    CommitRef we bail out — the PolicyStore is responsible
        //    for materialising those.
        //
        //    The tempfile is bound to this scope so it survives until
        //    `cedar authorize` has finished reading it.
        let policies_tmp;
        let policies_path: std::path::PathBuf = match source {
            EvaluatorSource::Inline { body } => {
                let tmp = tempfile::Builder::new()
                    .suffix(".cedar")
                    .tempfile()
                    .map_err(|e| ExternalError::Execution(format!("tempfile (policies): {e}")))?;
                std::fs::write(tmp.path(), body.as_bytes())
                    .map_err(|e| ExternalError::Execution(format!("write cedar policies: {e}")))?;
                let p = tmp.path().to_path_buf();
                policies_tmp = Some(tmp);
                p
            }
            EvaluatorSource::FilePath { path } => {
                policies_tmp = None;
                path.clone()
            }
            EvaluatorSource::CommitRef { .. } => {
                return Err(ExternalError::SourceResolution(
                    "commit_ref source not resolvable by the cedar runner; resolve via \
                     PolicyStore before dispatch"
                        .into(),
                ));
            }
        };
        let _ = &policies_tmp; // silence unused binding warning on the FilePath arm

        // 2. entities.json — single synthesized Agent entity.
        let entities = serde_json::json!([
            {
                "uid": { "type": "Agent", "id": agent_id },
                "attrs": {},
                "parents": []
            }
        ]);
        let entities_tmp = tempfile::Builder::new()
            .suffix(".entities.json")
            .tempfile()
            .map_err(|e| ExternalError::Execution(format!("tempfile (entities): {e}")))?;
        std::fs::write(
            entities_tmp.path(),
            serde_json::to_vec(&entities)
                .map_err(|e| ExternalError::Execution(format!("serialize entities: {e}")))?,
        )
        .map_err(|e| ExternalError::Execution(format!("write entities.json: {e}")))?;

        // 3. request.json.
        let request = serde_json::json!({
            "principal": format!("Agent::\"{agent_id}\""),
            "action":    format!("Action::\"{action}\""),
            "resource":  "Situation::\"default\"",
            "context":   { "situation": &situation.0 },
        });
        let request_tmp = tempfile::Builder::new()
            .suffix(".request.json")
            .tempfile()
            .map_err(|e| ExternalError::Execution(format!("tempfile (request): {e}")))?;
        std::fs::write(
            request_tmp.path(),
            serde_json::to_vec(&request)
                .map_err(|e| ExternalError::Execution(format!("serialize request: {e}")))?,
        )
        .map_err(|e| ExternalError::Execution(format!("write request.json: {e}")))?;

        // 4. Invoke `cedar authorize`.
        let output = Command::new(&self.cedar_path)
            .arg("authorize")
            .arg("--policies")
            .arg(&policies_path)
            .arg("--entities")
            .arg(entities_tmp.path())
            .arg("--request-json")
            .arg(request_tmp.path())
            .output()
            .map_err(|e| {
                if e.kind() == ErrorKind::NotFound {
                    ExternalError::Execution(format!(
                        "cedar binary not found at {}: {e}",
                        self.cedar_path
                    ))
                } else {
                    ExternalError::Execution(format!("failed to invoke {}: {e}", self.cedar_path))
                }
            })?;

        if !output.status.success() {
            return Err(ExternalError::Execution(format!(
                "cedar authorize exited {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        // 5. Parse JSON output.
        let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).map_err(|e| {
            ExternalError::Execution(format!(
                "parse cedar authorize output: {e} (stdout={})",
                String::from_utf8_lossy(&output.stdout)
            ))
        })?;

        let decision_str = envelope
            .get("decision")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let matched_policy: String = envelope
            .get("determining_policies")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .or_else(|| Some(v.to_string()))
            })
            .unwrap_or_else(|| "cedar".into());

        Ok(match decision_str {
            "Allow" => Decision::Allow {
                matched_policy,
                preconditions: vec![],
            },
            "Deny" => Decision::Deny {
                matched_policy,
                reason: "cedar authorize returned Deny".into(),
            },
            _ => Decision::NoPolicyMatch,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentstategraph_policy::selector::Situation;
    use std::collections::HashMap;

    /// Skip the enclosing test when the `cedar` binary is not on
    /// `$PATH`. Mirrors the Rego runner's `requires_opa!()`.
    macro_rules! requires_cedar {
        () => {
            if Command::new("cedar").arg("--help").output().is_err() {
                eprintln!("skipping: cedar binary not on $PATH");
                return;
            }
        };
    }

    fn empty_situation() -> Situation {
        Situation(HashMap::new())
    }

    #[test]
    fn inline_allow_happy_path() {
        requires_cedar!();
        let ev = CedarEvaluator::new();
        let src = EvaluatorSource::Inline {
            body: "permit(principal, action, resource);".into(),
        };
        let decision = ev
            .evaluate(&src, &empty_situation(), "read", "alice")
            .expect("cedar inline evaluation");
        match decision {
            Decision::Allow { .. } => {}
            other => panic!("expected Allow, got {other:?}"),
        }
    }

    #[test]
    fn file_path_source() {
        requires_cedar!();
        let tmp = tempfile::Builder::new()
            .suffix(".cedar")
            .tempfile()
            .expect("tempfile");
        std::fs::write(tmp.path(), b"permit(principal, action, resource);").expect("write policy");
        let ev = CedarEvaluator::new();
        let src = EvaluatorSource::FilePath {
            path: tmp.path().to_path_buf(),
        };
        let decision = ev
            .evaluate(&src, &empty_situation(), "read", "alice")
            .expect("cedar file evaluation");
        match decision {
            Decision::Allow { .. } => {}
            other => panic!("expected Allow, got {other:?}"),
        }
    }

    #[test]
    fn commit_ref_rejected_with_source_resolution() {
        // No cedar binary needed: CommitRef must short-circuit before
        // we even try to spawn.
        let ev = CedarEvaluator::new();
        let src = EvaluatorSource::CommitRef {
            path: "policy.cedar".into(),
        };
        let err = ev
            .evaluate(&src, &empty_situation(), "read", "alice")
            .expect_err("commit_ref must error");
        match err {
            ExternalError::SourceResolution(msg) => {
                assert!(
                    msg.contains("commit_ref"),
                    "expected commit_ref message, got: {msg}"
                );
            }
            other => panic!("expected SourceResolution, got {other:?}"),
        }
    }

    #[test]
    fn missing_binary_returns_execution_error() {
        let ev = CedarEvaluator::new_with_path("/definitely/not/a/real/path/to/cedar-binary-xyzzy");
        let src = EvaluatorSource::Inline {
            body: "permit(principal, action, resource);".into(),
        };
        let err = ev
            .evaluate(&src, &empty_situation(), "read", "alice")
            .expect_err("missing binary must error");
        match err {
            ExternalError::Execution(msg) => {
                assert!(
                    msg.contains("cedar binary not found"),
                    "expected 'cedar binary not found' in message, got: {msg}"
                );
            }
            other => panic!("expected Execution, got {other:?}"),
        }
    }
}
