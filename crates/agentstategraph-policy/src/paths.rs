//! Path construction helpers for the policy store.
//!
//! Policies live under `<prefix>/<domain>/<subdomain>/<slug>`. The active
//! ratified version lives at the base path; superseded versions move to
//! `<prefix>/<...>/history/<version>`.

use once_cell::sync::Lazy;
use regex::Regex;

use crate::error::PolicyError;

/// Reserved segment holding prior versions of a policy.
pub const HISTORY_KEY: &str = "history";

/// Reserved segment holding the policy's own JSON body when the policy
/// path has sub-paths underneath (i.e. `_meta` sibling to `history`).
pub const META_KEY: &str = "_meta";

/// Policy path segments must be lowercase alphanumeric with optional
/// hyphens. Paths themselves are `/`-separated with a leading slash and
/// at least one segment after the prefix.
static SEGMENT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[a-z0-9][a-z0-9_-]*$").expect("static segment regex"));

/// Validate a caller-supplied policy path (the portion under the store
/// prefix — e.g. `"infra/k8s/pod-failing"` or `"/infra/k8s/pod-failing"`).
/// Returns the normalized form without leading slash.
pub fn normalize(path: &str) -> Result<String, PolicyError> {
    let trimmed = path.trim_start_matches('/');
    if trimmed.is_empty() {
        return Err(PolicyError::InvalidPath(path.to_string()));
    }
    for seg in trimmed.split('/') {
        if !SEGMENT_RE.is_match(seg) {
            return Err(PolicyError::InvalidPath(path.to_string()));
        }
        if seg == HISTORY_KEY || seg == META_KEY {
            return Err(PolicyError::InvalidPath(path.to_string()));
        }
    }
    Ok(trimmed.to_string())
}

/// Storage path for the active/current policy JSON body.
pub fn active(prefix: &str, normalized: &str) -> String {
    format!("{}/{}/{}", prefix, normalized, META_KEY)
}

/// Storage path for a historical (superseded) version. The version
/// segment is prefixed with `v` so it is parsed as a map key, not a
/// list index — AgentStateGraph's path parser treats bare integers as
/// `PathComponent::Index`.
pub fn historical(prefix: &str, normalized: &str, version: u64) -> String {
    format!("{}/{}/{}/v{}", prefix, normalized, HISTORY_KEY, version)
}

/// Root for a single policy subtree — its `_meta` plus `history/*`.
pub fn policy_root(prefix: &str, normalized: &str) -> String {
    format!("{}/{}", prefix, normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_ok() {
        assert_eq!(
            normalize("/infra/k8s/pod-failing").unwrap(),
            "infra/k8s/pod-failing"
        );
        assert_eq!(
            normalize("infra/k8s/pod-failing").unwrap(),
            "infra/k8s/pod-failing"
        );
    }

    #[test]
    fn normalize_rejects_empty() {
        assert!(normalize("").is_err());
        assert!(normalize("/").is_err());
    }

    #[test]
    fn normalize_rejects_bad_chars() {
        assert!(normalize("/infra/K8S").is_err());
        assert!(normalize("/infra/../etc").is_err());
    }

    #[test]
    fn normalize_rejects_reserved() {
        assert!(normalize("/infra/history").is_err());
        assert!(normalize("/infra/_meta").is_err());
    }

    #[test]
    fn paths_compose() {
        assert_eq!(
            active("/policies", "infra/k8s/pod"),
            "/policies/infra/k8s/pod/_meta"
        );
        assert_eq!(
            historical("/policies", "infra/k8s/pod", 3),
            "/policies/infra/k8s/pod/history/v3"
        );
        assert_eq!(
            policy_root("/policies", "infra/k8s/pod"),
            "/policies/infra/k8s/pod"
        );
    }
}
