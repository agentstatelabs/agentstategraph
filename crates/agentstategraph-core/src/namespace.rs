//! Namespace primitive — the storage-layer isolation boundary for branches.
//!
//! A namespace scopes a set of branches. All ref operations are keyed on
//! `(namespace, branch_name)` at the storage layer, so two namespaces can
//! hold branches with the same name without collision, and cross-namespace
//! access is denied unless a policy explicitly permits it.
//!
//! The `"default"` namespace is the migration target for all pre-namespace
//! branch data and the fallback for single-namespace deployments.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Maximum length (in bytes) of a namespace name.
pub const MAX_NAMESPACE_LEN: usize = 64;

/// A validated namespace identifier.
///
/// Valid names contain only ASCII alphanumerics, hyphens (`-`), and
/// underscores (`_`), and are between 1 and 64 characters long.
/// The name `"default"` is valid and is the conventional namespace for
/// single-namespace deployments and migrated pre-namespace data.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Namespace(String);

impl Namespace {
    /// The conventional default namespace used for migration and single-ns
    /// deployments.
    pub const DEFAULT: &'static str = "default";

    /// Parse and validate a namespace name.
    pub fn new(name: impl Into<String>) -> Result<Self, NamespaceError> {
        let name = name.into();
        if name.is_empty() {
            return Err(NamespaceError::Empty);
        }
        if name.len() > MAX_NAMESPACE_LEN {
            return Err(NamespaceError::TooLong(name.len()));
        }
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(NamespaceError::InvalidChars(name));
        }
        Ok(Namespace(name))
    }

    /// Return the `"default"` namespace. Equivalent to
    /// `Namespace::new(Namespace::DEFAULT).unwrap()`.
    pub fn default_ns() -> Self {
        Namespace(Self::DEFAULT.to_string())
    }

    /// View the namespace name as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Namespace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for Namespace {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl FromStr for Namespace {
    type Err = NamespaceError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Namespace::new(s)
    }
}

impl TryFrom<String> for Namespace {
    type Error = NamespaceError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Namespace::new(s)
    }
}

impl TryFrom<&str> for Namespace {
    type Error = NamespaceError;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        Namespace::new(s)
    }
}

/// Errors produced by [`Namespace::new`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NamespaceError {
    #[error("namespace name must not be empty")]
    Empty,

    #[error("namespace name is too long ({0} bytes, max {MAX_NAMESPACE_LEN})")]
    TooLong(usize),

    #[error(
        "namespace name contains invalid characters: {0:?} \
         (only ASCII alphanumerics, hyphens, and underscores are allowed)"
    )]
    InvalidChars(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_names() {
        for name in ["default", "my-project", "proj_42", "a", "A1-b_C"] {
            assert!(Namespace::new(name).is_ok(), "expected valid: {name}");
        }
    }

    #[test]
    fn invalid_names() {
        assert_eq!(Namespace::new("").unwrap_err(), NamespaceError::Empty);
        assert!(matches!(
            Namespace::new("a".repeat(65)).unwrap_err(),
            NamespaceError::TooLong(_)
        ));
        for bad in ["has space", "dot.name", "slash/name", "colon:name", "bang!"] {
            assert!(
                matches!(
                    Namespace::new(bad).unwrap_err(),
                    NamespaceError::InvalidChars(_)
                ),
                "expected invalid: {bad}"
            );
        }
    }

    #[test]
    fn default_namespace_is_valid() {
        let ns = Namespace::default_ns();
        assert_eq!(ns.as_str(), "default");
        assert_eq!(Namespace::new(Namespace::DEFAULT).unwrap(), ns);
    }

    #[test]
    fn roundtrip_serde() {
        let ns = Namespace::new("my-project").unwrap();
        let json = serde_json::to_string(&ns).unwrap();
        assert_eq!(json, r#""my-project""#);
        let back: Namespace = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ns);
    }
}
