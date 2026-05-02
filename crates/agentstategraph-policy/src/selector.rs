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

    #[test]
    fn not_selector() {
        let s = sit(&[("flag", "off")]);
        assert!(Selector::negate(Selector::eq("flag", "on")).matches(&s));
        assert!(!Selector::negate(Selector::eq("flag", "off")).matches(&s));
    }

    #[test]
    fn all_empty_is_vacuously_true() {
        assert!(Selector::All { children: vec![] }.matches(&Situation::new()));
    }

    #[test]
    fn any_empty_is_false() {
        assert!(!Selector::Any { children: vec![] }.matches(&Situation::new()));
    }

    #[test]
    fn ne_false_when_key_missing() {
        let s = Situation::new();
        assert!(!Selector::Ne { key: "missing".into(), value: "x".into() }.matches(&s));
    }

    #[test]
    fn ne_true_when_value_differs() {
        let s = sit(&[("env", "staging")]);
        assert!(Selector::Ne { key: "env".into(), value: "prod".into() }.matches(&s));
    }

    #[test]
    fn ne_false_when_value_matches() {
        let s = sit(&[("env", "prod")]);
        assert!(!Selector::Ne { key: "env".into(), value: "prod".into() }.matches(&s));
    }

    #[test]
    fn numeric_missing_key_is_false_for_all_comparisons() {
        let s = Situation::new();
        assert!(!Selector::Gt { key: "n".into(), value: 0 }.matches(&s));
        assert!(!Selector::Gte { key: "n".into(), value: 0 }.matches(&s));
        assert!(!Selector::Lt { key: "n".into(), value: 0 }.matches(&s));
        assert!(!Selector::Lte { key: "n".into(), value: 0 }.matches(&s));
    }

    #[test]
    fn numeric_non_parseable_is_false() {
        let s = sit(&[("count", "not-a-number")]);
        assert!(!Selector::Gt { key: "count".into(), value: 0 }.matches(&s));
    }

    #[test]
    fn gte_and_lte_are_inclusive_at_boundary() {
        let s = sit(&[("n", "10")]);
        assert!(Selector::Gte { key: "n".into(), value: 10 }.matches(&s));
        assert!(Selector::Lte { key: "n".into(), value: 10 }.matches(&s));
        // exclusive counterparts at the boundary
        assert!(!Selector::Gt { key: "n".into(), value: 10 }.matches(&s));
        assert!(!Selector::Lt { key: "n".into(), value: 10 }.matches(&s));
    }

    #[test]
    fn regex_invalid_pattern_returns_false_not_panic() {
        let s = sit(&[("value", "anything")]);
        let sel = Selector::Matches { key: "value".into(), pattern: "[invalid".into() };
        assert!(!sel.matches(&s));
    }

    #[test]
    fn regex_missing_key_returns_false() {
        let s = Situation::new();
        let sel = Selector::Matches { key: "absent".into(), pattern: ".*".into() };
        assert!(!sel.matches(&s));
    }

    #[test]
    fn nested_all_any_not_complex_expression() {
        // (namespace==prod AND (state==Failed OR NOT healthy))
        let sel = Selector::all(vec![
            Selector::eq("namespace", "prod"),
            Selector::any(vec![
                Selector::eq("state", "Failed"),
                Selector::negate(Selector::exists("healthy")),
            ]),
        ]);

        // matches: prod + no healthy key
        let s1 = sit(&[("namespace", "prod")]);
        assert!(sel.matches(&s1));

        // matches: prod + state=Failed
        let s2 = sit(&[("namespace", "prod"), ("state", "Failed"), ("healthy", "true")]);
        assert!(sel.matches(&s2));

        // doesn't match: prod + state=Running + healthy present
        let s3 = sit(&[("namespace", "prod"), ("state", "Running"), ("healthy", "true")]);
        assert!(!sel.matches(&s3));

        // doesn't match: wrong namespace
        let s4 = sit(&[("namespace", "dev")]);
        assert!(!sel.matches(&s4));
    }

    #[test]
    fn situation_with_builder_chain() {
        let s = Situation::new()
            .with("a", "1")
            .with("b", "2");
        assert_eq!(s.get("a").unwrap(), "1");
        assert_eq!(s.get("b").unwrap(), "2");
        assert!(s.get("c").is_none());
    }

    #[test]
    fn situation_from_hashmap() {
        use std::collections::HashMap;
        let mut m = HashMap::new();
        m.insert("x".to_string(), "y".to_string());
        let s: Situation = m.into();
        assert_eq!(s.get("x").unwrap(), "y");
    }

    #[test]
    fn all_variants_roundtrip_json() {
        let selectors = vec![
            Selector::Always,
            Selector::Never,
            Selector::Eq { key: "k".into(), value: "v".into() },
            Selector::Ne { key: "k".into(), value: "v".into() },
            Selector::Matches { key: "k".into(), pattern: ".*".into() },
            Selector::Exists { key: "k".into() },
            Selector::Gt { key: "k".into(), value: 5 },
            Selector::Gte { key: "k".into(), value: 5 },
            Selector::Lt { key: "k".into(), value: 5 },
            Selector::Lte { key: "k".into(), value: 5 },
            Selector::Not { child: Box::new(Selector::Always) },
            Selector::All { children: vec![Selector::Always] },
            Selector::Any { children: vec![Selector::Never] },
        ];
        for sel in selectors {
            let j = serde_json::to_value(&sel).unwrap();
            let back: Selector = serde_json::from_value(j).unwrap();
            assert_eq!(sel, back);
        }
    }
}
