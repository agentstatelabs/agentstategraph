//! Situation selector grammar.
//!
//! A `Situation` is an opaque map of fact-name → fact-value. A `Selector`
//! is a boolean tree over that map. Selectors serialize as tagged-enum
//! JSON so they are legible and machine-evaluable without a separate DSL
//! parser (that may come later).

use std::collections::HashMap;

use regex::Regex;
use serde::{Deserialize, Serialize};

/// Facts describing the current environment. Keys and values are opaque
/// strings — the consumer decides the schema.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct Situation(pub HashMap<String, String>);

impl Situation {
    pub fn new() -> Self {
        Self(HashMap::new())
    }

    pub fn with(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.0.insert(key.into(), value.into());
        self
    }

    pub fn get(&self, key: &str) -> Option<&String> {
        self.0.get(key)
    }
}

impl From<HashMap<String, String>> for Situation {
    fn from(m: HashMap<String, String>) -> Self {
        Situation(m)
    }
}

/// Boolean expression over a `Situation`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Selector {
    /// Matches everything. Useful for broad-scope policies.
    Always,
    /// Matches nothing. Useful for placeholders and tests.
    Never,
    /// All children match.
    All { children: Vec<Selector> },
    /// At least one child matches.
    Any { children: Vec<Selector> },
    /// The child does not match.
    Not { child: Box<Selector> },
    /// `situation[key] == value`.
    Eq { key: String, value: String },
    /// `situation[key] != value`. False if key is missing.
    Ne { key: String, value: String },
    /// `situation[key]` matches the given regex.
    Matches { key: String, pattern: String },
    /// Key is present (regardless of value).
    Exists { key: String },
    /// Numeric `situation[key] > value`. False if key is missing or
    /// not parseable as i64.
    Gt { key: String, value: i64 },
    /// Numeric `>=`.
    Gte { key: String, value: i64 },
    /// Numeric `<`.
    Lt { key: String, value: i64 },
    /// Numeric `<=`.
    Lte { key: String, value: i64 },
}

impl Selector {
    pub fn matches(&self, s: &Situation) -> bool {
        match self {
            Selector::Always => true,
            Selector::Never => false,
            Selector::All { children } => children.iter().all(|c| c.matches(s)),
            Selector::Any { children } => children.iter().any(|c| c.matches(s)),
            Selector::Not { child } => !child.matches(s),
            Selector::Eq { key, value } => s.get(key).is_some_and(|v| v == value),
            Selector::Ne { key, value } => s.get(key).is_some_and(|v| v != value),
            Selector::Matches { key, pattern } => match Regex::new(pattern) {
                Ok(re) => s.get(key).is_some_and(|v| re.is_match(v)),
                Err(_) => false,
            },
            Selector::Exists { key } => s.get(key).is_some(),
            Selector::Gt { key, value } => numeric(s, key).is_some_and(|n| n > *value),
            Selector::Gte { key, value } => numeric(s, key).is_some_and(|n| n >= *value),
            Selector::Lt { key, value } => numeric(s, key).is_some_and(|n| n < *value),
            Selector::Lte { key, value } => numeric(s, key).is_some_and(|n| n <= *value),
        }
    }

    pub fn all(children: Vec<Selector>) -> Self {
        Selector::All { children }
    }

    pub fn any(children: Vec<Selector>) -> Self {
        Selector::Any { children }
    }

    pub fn negate(child: Selector) -> Self {
        Selector::Not {
            child: Box::new(child),
        }
    }

    pub fn eq(key: impl Into<String>, value: impl Into<String>) -> Self {
        Selector::Eq {
            key: key.into(),
            value: value.into(),
        }
    }

    pub fn exists(key: impl Into<String>) -> Self {
        Selector::Exists { key: key.into() }
    }
}

fn numeric(s: &Situation, key: &str) -> Option<i64> {
    s.get(key).and_then(|v| v.parse::<i64>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sit(pairs: &[(&str, &str)]) -> Situation {
        let mut s = Situation::new();
        for (k, v) in pairs {
            s = s.with(*k, *v);
        }
        s
    }

    #[test]
    fn always_and_never() {
        assert!(Selector::Always.matches(&Situation::new()));
        assert!(!Selector::Never.matches(&Situation::new()));
    }

    #[test]
    fn eq_ne_exists() {
        let s = sit(&[("namespace", "prod")]);
        assert!(Selector::eq("namespace", "prod").matches(&s));
        assert!(!Selector::eq("namespace", "dev").matches(&s));
        assert!(Selector::exists("namespace").matches(&s));
        assert!(!Selector::exists("region").matches(&s));
    }

    #[test]
    fn numeric_comparisons() {
        let s = sit(&[("count", "5")]);
        assert!(
            (Selector::Gt {
                key: "count".into(),
                value: 3
            })
            .matches(&s)
        );
        assert!(
            !(Selector::Gt {
                key: "count".into(),
                value: 10
            })
            .matches(&s)
        );
        assert!(
            (Selector::Lte {
                key: "count".into(),
                value: 5
            })
            .matches(&s)
        );
    }

    #[test]
    fn regex_matches() {
        let s = sit(&[("state", "CrashLoopBackOff")]);
        let sel = Selector::Matches {
            key: "state".into(),
            pattern: "^Crash".into(),
        };
        assert!(sel.matches(&s));
    }

    #[test]
    fn selector_roundtrips_json() {
        let sel = Selector::all(vec![
            Selector::eq("namespace", "prod"),
            Selector::any(vec![
                Selector::eq("state", "Failed"),
                Selector::negate(Selector::exists("healthy")),
            ]),
        ]);
        let json = serde_json::to_value(&sel).unwrap();
        let back: Selector = serde_json::from_value(json).unwrap();
        assert_eq!(sel, back);
    }
}
