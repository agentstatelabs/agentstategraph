//! Pure check algorithm: given a candidate taint list + request
//! context, produce a [`TaintCheck`] answering "can this write
//! proceed?".
//!
//! This function is deliberately pure — storage fetch happens in
//! the caller (`Repository::check_taint`), so the algorithm can be
//! exercised with hand-constructed candidate lists in tests
//! without a database.

use chrono::{DateTime, Utc};

use crate::types::{Taint, TaintCheck, TaintEffect, TaintKind};

/// Required minimum commit confidence to pass a review-effect
/// taint. Hardcoded at 0.9 per spec/TAINT_SPEC.md §"Effects".
pub const REVIEW_CONFIDENCE_THRESHOLD: f64 = 0.9;

/// Filter a candidate set down to taints whose `path` is either
/// equal to `request_path` or a propagating ancestor of it.
///
/// Callers typically get this list from
/// `Storage::check_taint(request_path)`, which already filters
/// server-side; this helper exists so in-memory and test harnesses
/// can run the same algorithm without a Storage impl.
pub fn ancestor_candidates<'a>(
    request_path: &str,
    candidates: &'a [Taint],
    now: DateTime<Utc>,
) -> Vec<&'a Taint> {
    candidates
        .iter()
        .filter(|t| {
            if !t.is_active(now) {
                return false;
            }
            if t.path == request_path {
                return true;
            }
            if !t.propagate {
                return false;
            }
            // ancestor prefix match: candidate path must end in a
            // boundary so `/a/b` does not match `/a/banana`.
            let prefix = if t.path.ends_with('/') {
                t.path.clone()
            } else {
                format!("{}/", t.path)
            };
            request_path.starts_with(&prefix)
        })
        .collect()
}

/// Evaluate access for `(request_path, agent_id, confidence)` against
/// a candidate taint list. Returns the aggregated [`TaintCheck`].
///
/// Precedence (strongest first):
///
/// 1. Any active `Quarantine` not authorizing `agent_id` →
///    `can_write = false`.
/// 2. Any active `Taint` with effect `Block` → `can_write = false`.
/// 3. Any active `Taint` with effect `Review` →
///    `required_confidence = max(required_confidence, 0.9)`;
///    `can_write = confidence >= required_confidence`.
/// 4. Any active `Taint` with effect `Isolate` → advisory;
///    `isolated = true` for query-filtering purposes.
/// 5. `Warn`-effect taints + all watches are advisory.
pub fn evaluate_access(
    request_path: &str,
    agent_id: &str,
    confidence: f64,
    candidates: &[Taint],
    now: DateTime<Utc>,
) -> TaintCheck {
    let mut out = TaintCheck::clear();

    for t in ancestor_candidates(request_path, candidates, now) {
        match t.kind {
            TaintKind::Taint => {
                out.tainted = true;
                out.taints.push(t.clone());
                match t.effect {
                    TaintEffect::Block => {
                        out.can_write = false;
                    }
                    TaintEffect::Review => {
                        if out.required_confidence < REVIEW_CONFIDENCE_THRESHOLD {
                            out.required_confidence = REVIEW_CONFIDENCE_THRESHOLD;
                        }
                    }
                    TaintEffect::Isolate => {
                        out.isolated = true;
                    }
                    TaintEffect::Warn | TaintEffect::Advisory => {
                        // advisory
                    }
                }
            }
            TaintKind::Quarantine => {
                out.quarantined = true;
                out.quarantines.push(t.clone());
                let authorized = t.authorized_agents();
                // union into the aggregated allowlist
                for a in &authorized {
                    if !out.authorized_agents.contains(a) {
                        out.authorized_agents.push(a.clone());
                    }
                }
                if !authorized.iter().any(|a| a == agent_id) {
                    out.can_write = false;
                }
            }
            TaintKind::Watch => {
                out.watched = true;
                out.watches.push(t.clone());
                // purely advisory
            }
        }
    }

    // Confidence gate — after collecting all review taints.
    if out.can_write && confidence < out.required_confidence {
        out.can_write = false;
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{TaintMetadata, TaintSeverity};

    fn make(
        path: &str,
        name: &str,
        kind: TaintKind,
        effect: TaintEffect,
        propagate: bool,
    ) -> Taint {
        Taint {
            id: format!("t-{name}"),
            path: path.into(),
            name: name.into(),
            kind,
            effect,
            severity: TaintSeverity::Medium,
            reason: "test".into(),
            agent_id: "test-agent".into(),
            commit_id: String::new(),
            created_at: Utc::now(),
            expires_at: None,
            resolved_at: None,
            resolved_by: None,
            resolved_reason: None,
            resolved_proof: None,
            propagate,
            metadata: TaintMetadata::new(),
        }
    }

    fn quarantine(path: &str, name: &str, allowed: &[&str]) -> Taint {
        let mut t = make(path, name, TaintKind::Quarantine, TaintEffect::Block, true);
        t.metadata.insert(
            "authorized_agents",
            serde_json::json!(allowed.iter().map(|s| s.to_string()).collect::<Vec<_>>()),
        );
        t
    }

    #[test]
    fn empty_candidates_allows_write() {
        let c = evaluate_access("/x", "agent-1", 0.5, &[], Utc::now());
        assert!(c.can_write);
        assert!(!c.tainted && !c.quarantined && !c.watched);
        assert_eq!(c.required_confidence, 0.0);
    }

    #[test]
    fn block_effect_denies_write() {
        let ts = vec![make(
            "/cluster",
            "down",
            TaintKind::Taint,
            TaintEffect::Block,
            true,
        )];
        let c = evaluate_access("/cluster/nodes/a", "agent-1", 1.0, &ts, Utc::now());
        assert!(c.tainted);
        assert!(!c.can_write);
    }

    #[test]
    fn review_effect_requires_high_confidence() {
        let ts = vec![make(
            "/cluster",
            "unstable",
            TaintKind::Taint,
            TaintEffect::Review,
            true,
        )];
        let c_low = evaluate_access("/cluster/x", "agent-1", 0.5, &ts, Utc::now());
        assert_eq!(c_low.required_confidence, 0.9);
        assert!(!c_low.can_write);
        let c_high = evaluate_access("/cluster/x", "agent-1", 0.95, &ts, Utc::now());
        assert!(c_high.can_write);
    }

    #[test]
    fn review_respects_full_confidence_boundary() {
        let ts = vec![make(
            "/x",
            "rev",
            TaintKind::Taint,
            TaintEffect::Review,
            true,
        )];
        // Exactly 0.9 passes (>= threshold).
        assert!(evaluate_access("/x", "a", 0.9, &ts, Utc::now()).can_write);
        // 0.89999 fails.
        assert!(!evaluate_access("/x", "a", 0.89999, &ts, Utc::now()).can_write);
    }

    #[test]
    fn quarantine_blocks_unauthorized() {
        let ts = vec![quarantine("/cluster", "sec", &["agent/security"])];
        let c = evaluate_access("/cluster/a", "agent-1", 1.0, &ts, Utc::now());
        assert!(c.quarantined);
        assert!(!c.can_write);
        assert_eq!(c.authorized_agents, vec!["agent/security".to_string()]);
    }

    #[test]
    fn quarantine_passes_authorized() {
        let ts = vec![quarantine("/cluster", "sec", &["agent/security"])];
        let c = evaluate_access("/cluster/a", "agent/security", 1.0, &ts, Utc::now());
        assert!(c.quarantined);
        assert!(c.can_write);
    }

    #[test]
    fn ancestor_taint_propagates() {
        let ts = vec![make(
            "/cluster/nodes/picoup2",
            "disk",
            TaintKind::Taint,
            TaintEffect::Warn,
            true,
        )];
        let c = evaluate_access(
            "/cluster/nodes/picoup2/services/spark",
            "a",
            1.0,
            &ts,
            Utc::now(),
        );
        assert!(c.tainted);
    }

    #[test]
    fn non_propagating_ancestor_does_not_apply() {
        let ts = vec![make(
            "/cluster",
            "leaf",
            TaintKind::Taint,
            TaintEffect::Block,
            false,
        )];
        let c = evaluate_access("/cluster/nodes/a", "a", 1.0, &ts, Utc::now());
        assert!(!c.tainted);
        assert!(c.can_write);
    }

    #[test]
    fn ancestor_prefix_match_respects_path_boundary() {
        // `/cluster-staging` must NOT match a taint on `/cluster`.
        let ts = vec![make(
            "/cluster",
            "x",
            TaintKind::Taint,
            TaintEffect::Block,
            true,
        )];
        let c = evaluate_access("/cluster-staging/a", "a", 1.0, &ts, Utc::now());
        assert!(!c.tainted);
    }

    #[test]
    fn resolved_taint_is_ignored() {
        let mut t = make("/x", "rev", TaintKind::Taint, TaintEffect::Block, true);
        t.resolved_at = Some(Utc::now() - chrono::Duration::seconds(1));
        let c = evaluate_access("/x", "a", 1.0, &[t], Utc::now());
        assert!(!c.tainted);
        assert!(c.can_write);
    }

    #[test]
    fn expired_taint_is_ignored() {
        let mut t = make("/x", "rev", TaintKind::Taint, TaintEffect::Block, true);
        t.expires_at = Some(Utc::now() - chrono::Duration::seconds(1));
        let c = evaluate_access("/x", "a", 1.0, &[t], Utc::now());
        assert!(!c.tainted);
        assert!(c.can_write);
    }

    #[test]
    fn isolate_effect_flags_visibility_without_blocking_writes() {
        let ts = vec![make(
            "/secret",
            "iso",
            TaintKind::Taint,
            TaintEffect::Isolate,
            true,
        )];
        let c = evaluate_access("/secret/x", "a", 1.0, &ts, Utc::now());
        assert!(c.tainted);
        assert!(c.isolated);
        assert!(c.can_write);
    }

    #[test]
    fn watch_is_advisory_only() {
        let ts = vec![make(
            "/cluster",
            "perf",
            TaintKind::Watch,
            TaintEffect::Advisory,
            true,
        )];
        let c = evaluate_access("/cluster/x", "a", 0.0, &ts, Utc::now());
        assert!(c.watched);
        assert!(c.can_write);
        assert_eq!(c.required_confidence, 0.0);
    }

    #[test]
    fn block_trumps_review() {
        // Mixed candidates: block wins even when a review taint
        // exists. Confidence cannot rescue a Block.
        let ts = vec![
            make(
                "/cluster",
                "rev",
                TaintKind::Taint,
                TaintEffect::Review,
                true,
            ),
            make(
                "/cluster",
                "blk",
                TaintKind::Taint,
                TaintEffect::Block,
                true,
            ),
        ];
        let c = evaluate_access("/cluster/x", "a", 1.0, &ts, Utc::now());
        assert!(!c.can_write);
    }
}
