//! Path addressing for navigating state trees.
//!
//! Paths use a JSON-path-like syntax:
//!   /                       → root node
//!   /nodes                  → "nodes" key in root map
//!   /nodes/0                → first element of "nodes" list
//!   /nodes/0/hostname       → "hostname" key in first node object

use serde::{Deserialize, Serialize};
use std::fmt;

/// A component of a path — either a map key or a list/set index.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PathComponent {
    Key(String),
    Index(usize),
}

/// A path into a state tree.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StatePath {
    components: Vec<PathComponent>,
}

impl StatePath {
    /// The root path (empty components).
    pub fn root() -> Self {
        Self {
            components: Vec::new(),
        }
    }

    /// Parse a path from a string like "/nodes/0/hostname".
    pub fn parse(s: &str) -> Result<Self, PathError> {
        if s.is_empty() || s == "/" {
            return Ok(Self::root());
        }

        let s = s.strip_prefix('/').ok_or(PathError::MustStartWithSlash)?;
        let components = s
            .split('/')
            .map(|segment| {
                if segment.is_empty() {
                    Err(PathError::EmptySegment)
                } else if let Ok(index) = segment.parse::<usize>() {
                    Ok(PathComponent::Index(index))
                } else {
                    Ok(PathComponent::Key(segment.to_string()))
                }
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self { components })
    }

    /// Return the path components.
    pub fn components(&self) -> &[PathComponent] {
        &self.components
    }

    /// Whether this is the root path.
    pub fn is_root(&self) -> bool {
        self.components.is_empty()
    }

    /// Return the parent path (or None if this is root).
    pub fn parent(&self) -> Option<Self> {
        if self.components.is_empty() {
            None
        } else {
            Some(Self {
                components: self.components[..self.components.len() - 1].to_vec(),
            })
        }
    }

    /// Return the last component (or None if this is root).
    pub fn last(&self) -> Option<&PathComponent> {
        self.components.last()
    }

    /// Append a key component.
    pub fn push_key(&self, key: impl Into<String>) -> Self {
        let mut components = self.components.clone();
        components.push(PathComponent::Key(key.into()));
        Self { components }
    }

    /// Append an index component.
    pub fn push_index(&self, index: usize) -> Self {
        let mut components = self.components.clone();
        components.push(PathComponent::Index(index));
        Self { components }
    }

    /// Number of components in this path.
    pub fn depth(&self) -> usize {
        self.components.len()
    }
}

impl fmt::Display for StatePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.components.is_empty() {
            write!(f, "/")
        } else {
            for component in &self.components {
                match component {
                    PathComponent::Key(k) => write!(f, "/{}", k)?,
                    PathComponent::Index(i) => write!(f, "/{}", i)?,
                }
            }
            Ok(())
        }
    }
}

/// Errors that can occur when parsing a path.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PathError {
    #[error("path must start with '/'")]
    MustStartWithSlash,
    #[error("path contains an empty segment")]
    EmptySegment,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_root() {
        assert_eq!(StatePath::parse("/").unwrap(), StatePath::root());
        assert_eq!(StatePath::parse("").unwrap(), StatePath::root());
    }

    #[test]
    fn test_parse_simple_key() {
        let path = StatePath::parse("/nodes").unwrap();
        assert_eq!(path.components(), &[PathComponent::Key("nodes".to_string())]);
    }

    #[test]
    fn test_parse_nested() {
        let path = StatePath::parse("/nodes/0/hostname").unwrap();
        assert_eq!(
            path.components(),
            &[
                PathComponent::Key("nodes".to_string()),
                PathComponent::Index(0),
                PathComponent::Key("hostname".to_string()),
            ]
        );
    }

    #[test]
    fn test_display_roundtrip() {
        let path = StatePath::parse("/nodes/0/hostname").unwrap();
        assert_eq!(path.to_string(), "/nodes/0/hostname");
    }

    #[test]
    fn test_parent() {
        let path = StatePath::parse("/nodes/0/hostname").unwrap();
        let parent = path.parent().unwrap();
        assert_eq!(parent.to_string(), "/nodes/0");
    }

    #[test]
    fn test_root_has_no_parent() {
        assert!(StatePath::root().parent().is_none());
    }

    #[test]
    fn test_push() {
        let path = StatePath::root().push_key("nodes").push_index(0).push_key("hostname");
        assert_eq!(path.to_string(), "/nodes/0/hostname");
    }

    #[test]
    fn test_error_no_leading_slash() {
        assert!(StatePath::parse("nodes").is_err());
    }

    #[test]
    fn test_error_empty_segment() {
        assert!(StatePath::parse("/nodes//hostname").is_err());
    }
}
