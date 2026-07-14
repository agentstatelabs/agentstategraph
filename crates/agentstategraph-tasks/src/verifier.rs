//! Proof verification surface.
//!
//! The crate defines the shape; concrete implementations live in
//! consumers (a code/CI consumer ships a `GitFileTestVerifier`,
//! ThreadWeaver a `ChatVerifier`, etc.). A `NoopVerifier` is included for fallbacks
//! and tests.
//!
//! # Framing: `Unverifiable` is a flag, not a verdict
//!
//! `VerifyResult::Unverifiable` is a **non-blocking flag**, not a
//! success verdict. Consumers that treat it as "passed" are wrong —
//! it simply means the proof kind (e.g. a freeform text attestation)
//! can't be mechanically checked by the current verifier.
//!
//! For go/no-go decisions use [`VerifyReport::all_strongly_verified`],
//! which is true iff every entry is `Verified`. For a human- or
//! LLM-readable one-liner use [`VerifyReport::summary`], which
//! explicitly distinguishes verified, decayed, and unverifiable
//! counts and flags that unverifiable is not "safe to ignore."

use crate::types::{Proof, TaskId};

pub trait Verifier {
    /// Verify a single proof. Never fails — unknown kinds return
    /// `VerifyResult::Unverifiable`.
    fn verify(&self, proof: &Proof) -> VerifyResult;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyResult {
    /// Proof holds.
    Verified { message: String },
    /// Proof was valid when recorded but has since decayed.
    Decayed { reason: String },
    /// Proof kind can't be mechanically verified (e.g. text proofs).
    /// Not a failure — just flagged.
    Unverifiable { reason: String },
}

/// A no-op verifier that reports every proof as Unverifiable.
/// Useful as a fallback or in tests.
pub struct NoopVerifier;

impl Verifier for NoopVerifier {
    fn verify(&self, _: &Proof) -> VerifyResult {
        VerifyResult::Unverifiable {
            reason: "no verifier configured".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyEntry {
    pub task_id: TaskId,
    pub result: VerifyResult,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyReport {
    pub plan: String,
    pub results: Vec<VerifyEntry>,
}

impl VerifyReport {
    pub fn verified_count(&self) -> usize {
        self.results
            .iter()
            .filter(|r| matches!(r.result, VerifyResult::Verified { .. }))
            .count()
    }

    pub fn decayed_count(&self) -> usize {
        self.results
            .iter()
            .filter(|r| matches!(r.result, VerifyResult::Decayed { .. }))
            .count()
    }

    pub fn unverifiable_count(&self) -> usize {
        self.results
            .iter()
            .filter(|r| matches!(r.result, VerifyResult::Unverifiable { .. }))
            .count()
    }

    /// True iff the report is non-empty AND every entry is
    /// `VerifyResult::Verified`. Use this for go/no-go decisions —
    /// `Unverifiable` entries do NOT count as passing.
    pub fn all_strongly_verified(&self) -> bool {
        !self.results.is_empty()
            && self
                .results
                .iter()
                .all(|r| matches!(r.result, VerifyResult::Verified { .. }))
    }

    /// True iff at least one entry is `VerifyResult::Unverifiable`.
    /// Unverifiable is a non-blocking flag — it's not a failure, but
    /// it is NOT safe to treat as success either.
    pub fn has_unverifiable(&self) -> bool {
        self.results
            .iter()
            .any(|r| matches!(r.result, VerifyResult::Unverifiable { .. }))
    }

    /// Human- or LLM-readable one-line summary of the report.
    ///
    /// Distinguishes the three categories and explicitly flags that
    /// `unverifiable` is NOT "safe to ignore" — it's a human
    /// attestation, not a mechanical check.
    pub fn summary(&self) -> String {
        let verified = self.verified_count();
        let decayed = self.decayed_count();
        let unverifiable = self.unverifiable_count();
        format!(
            "{verified} verified (mechanically checked); {decayed} decayed (proof no longer holds); {unverifiable} unverifiable (text proof — human attestation, not mechanically checked; do NOT treat as success)."
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Proof, ProofKind};

    #[test]
    fn noop_verifier_returns_unverifiable() {
        let v = NoopVerifier;
        let p = Proof::commit("abc");
        assert!(matches!(v.verify(&p), VerifyResult::Unverifiable { .. }));
    }

    #[test]
    fn report_counts() {
        let report = VerifyReport {
            plan: "p".to_string(),
            results: vec![
                VerifyEntry {
                    task_id: TaskId::new(1),
                    result: VerifyResult::Verified {
                        message: "ok".into(),
                    },
                },
                VerifyEntry {
                    task_id: TaskId::new(2),
                    result: VerifyResult::Decayed {
                        reason: "file deleted".into(),
                    },
                },
                VerifyEntry {
                    task_id: TaskId::new(3),
                    result: VerifyResult::Unverifiable {
                        reason: "text".into(),
                    },
                },
            ],
        };
        assert_eq!(report.verified_count(), 1);
        assert_eq!(report.decayed_count(), 1);
        assert_eq!(report.unverifiable_count(), 1);
        assert!(!report.all_strongly_verified());
        assert!(report.has_unverifiable());
    }

    #[test]
    fn summary_distinguishes_three_categories() {
        let report = VerifyReport {
            plan: "p".to_string(),
            results: vec![
                VerifyEntry {
                    task_id: TaskId::new(1),
                    result: VerifyResult::Verified {
                        message: "ok".into(),
                    },
                },
                VerifyEntry {
                    task_id: TaskId::new(2),
                    result: VerifyResult::Verified {
                        message: "ok".into(),
                    },
                },
                VerifyEntry {
                    task_id: TaskId::new(3),
                    result: VerifyResult::Decayed {
                        reason: "file missing".into(),
                    },
                },
                VerifyEntry {
                    task_id: TaskId::new(4),
                    result: VerifyResult::Unverifiable {
                        reason: "text".into(),
                    },
                },
            ],
        };
        let s = report.summary();
        // All three categories explicitly named.
        assert!(s.contains("verified"), "summary: {s}");
        assert!(s.contains("decayed"), "summary: {s}");
        assert!(s.contains("unverifiable"), "summary: {s}");
        // Counts present.
        assert!(
            s.contains('2'),
            "summary should mention the 2 verified: {s}"
        );
        // Explicit warning that unverifiable is not success.
        assert!(
            s.to_lowercase().contains("not")
                && (s.to_lowercase().contains("success")
                    || s.to_lowercase().contains("mechanically checked")),
            "summary should flag unverifiable is not safe to treat as success: {s}"
        );
    }

    #[test]
    fn all_strongly_verified_rejects_unverifiable() {
        let report = VerifyReport {
            plan: "p".to_string(),
            results: vec![
                VerifyEntry {
                    task_id: TaskId::new(1),
                    result: VerifyResult::Verified {
                        message: "ok".into(),
                    },
                },
                VerifyEntry {
                    task_id: TaskId::new(2),
                    result: VerifyResult::Unverifiable {
                        reason: "text".into(),
                    },
                },
            ],
        };
        assert!(!report.all_strongly_verified());
        assert!(report.has_unverifiable());
    }

    // Ensure ProofKind is used in test scope (silences unused warning if the
    // set of tests is trimmed).
    #[allow(dead_code)]
    fn _touch_kinds() -> ProofKind {
        ProofKind::Text
    }
}
