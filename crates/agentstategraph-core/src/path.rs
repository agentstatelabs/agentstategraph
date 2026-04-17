//! Path addressing for navigating state trees.
//!
//! Paths use a JSON-path-like syntax:
//!   /                       → root node
//!   /nodes                  → "nodes" key in root map
//!   /nodes/0                → first element of "nodes" list
//!   /nodes/0/hostname       → "hostname" key in first node object

use serde::{Deserialize, Serialize};
use std::fmt;

/// Maximum number of path components accepted by `StatePath::parse`.
/// Prevents pathological deeply-nested user input from blowing the stack
/// or causing quadratic work in tree walkers.
pub const MAX_PATH_DEPTH: usize = 64;

/// Maximum length (in bytes) of a single path segment accepted by
/// `StatePath::parse`. Keys longer than this are almost always attacks
/// or bugs — real keys are short identifiers.
pub const MAX_SEGMENT_LEN: usize = 4096;

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
            .enumerate()
            .map(|(i, segment)| {
                if segment.is_empty() {
                    Err(PathError::EmptySegment)
                } else if segment.len() > MAX_SEGMENT_LEN {
                    Err(PathError::SegmentTooLong {
                        index: i,
                        len: segment.len(),
                    })
                } else if let Ok(index) = segment.parse::<usize>() {
                    Ok(PathComponent::Index(index))
                } else {
                    Ok(PathComponent::Key(segment.to_string()))
                }
            })
            .collect::<Result<Vec<_>, _>>()?;

        if components.len() > MAX_PATH_DEPTH {
            return Err(PathError::TooDeep {
                depth: components.len(),
            });
        }

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
    #[error("path is too deep: {depth} components (max {})", MAX_PATH_DEPTH)]
    TooDeep { depth: usize },
    #[error(
        "path segment at index {index} is too long: {len} bytes (max {})",
        MAX_SEGMENT_LEN
    )]
    SegmentTooLong { index: usize, len: usize },
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
        assert_eq!(
            path.components(),
            &[PathComponent::Key("nodes".to_string())]
        );
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
        let path = StatePath::root()
            .push_key("nodes")
            .push_index(0)
            .push_key("hostname");
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

    #[test]
    fn test_error_too_deep() {
        let mut s = String::new();
        for i in 0..(MAX_PATH_DEPTH + 5) {
            s.push_str(&format!("/k{}", i));
        }
        match StatePath::parse(&s) {
            Err(PathError::TooDeep { depth }) => {
                assert_eq!(depth, MAX_PATH_DEPTH + 5);
            }
            other => panic!("expected TooDeep, got {:?}", other),
        }
    }

    #[test]
    fn test_error_segment_too_long() {
        let big = "x".repeat(MAX_SEGMENT_LEN + 1);
        let s = format!("/a/{}", big);
        match StatePath::parse(&s) {
            Err(PathError::SegmentTooLong { index, len }) => {
                assert_eq!(index, 1);
                assert_eq!(len, MAX_SEGMENT_LEN + 1);
            }
            other => panic!("expected SegmentTooLong, got {:?}", other),
        }
    }

    #[test]
    fn test_caps_boundary_accepts() {
        // Exactly at the cap should be accepted.
        let seg = "x".repeat(MAX_SEGMENT_LEN);
        let s = format!("/{}", seg);
        assert!(StatePath::parse(&s).is_ok());

        let mut deep = String::new();
        for i in 0..MAX_PATH_DEPTH {
            deep.push_str(&format!("/k{}", i));
        }
        assert!(StatePath::parse(&deep).is_ok());
    }
}
