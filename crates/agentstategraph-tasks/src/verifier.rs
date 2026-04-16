//! Proof verification surface.
//!
//! The crate defines the shape; concrete implementations live in
//! consumers (CTXone ships a `GitFileTestVerifier`, ThreadWeaver a
//! `ChatVerifier`, etc.). A `NoopVerifier` is included for fallbacks
//! and tests.

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

    pub fn is_all_verified(&self) -> bool {
        !self.results.is_empty()
            && self
                .results
                .iter()
                .all(|r| matches!(r.result, VerifyResult::Verified { .. }))
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
        assert!(!report.is_all_verified());
    }

    // Ensure ProofKind is used in test scope (silences unused warning if the
    // set of tests is trimmed).
    #[allow(dead_code)]
    fn _touch_kinds() -> ProofKind {
        ProofKind::Text
    }
}
