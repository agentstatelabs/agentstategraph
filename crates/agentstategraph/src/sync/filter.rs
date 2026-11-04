//! What a push refuses to send (DESIGN.md Plan G t-011, with t-005's
//! guards).
//!
//! Before a push sends anything, every value its new commits add is put to
//! a list of [`PushFilter`]s, each of which can name rules the value
//! breaks. A value is checked at each path a new commit's tree adds it,
//! against that commit's first parent. A finding blocks the push unless the
//! caller overrides it by id, with a reason ([`PushOverride`]), which goes to
//! the remote with the push for its push log.
//!
//! Two rules always apply and can't be overridden:
//!
//! - **Secrets never sync.** A push whose new commits hold anything under
//!   `/_meta/_secret` is refused, and so is one a remote receives.
//! - **Local-only refs stay local.** A ref whose tree holds `true` at
//!   [`LOCAL_ONLY_PATH`] (an embedder's regenerable index, say) can't be
//!   pushed, and a remote doesn't offer its own.
//!
//! The built-in [`CredentialFilter`] looks for provider token and key
//! shapes, private keys, and password assignments, in values and in the
//! keys along their paths. An embedder adds its own filters through
//! [`PushOptions`].

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use agentstategraph_core::{Atom, IntentCategory, Node, Object, ObjectId, StatePath};
use agentstategraph_storage::Storage;
use regex::{Regex, RegexSet};
use serde::{Deserialize, Serialize};

use crate::repo::{CommitOptions, META_SECRET_PREFIX, RepoError, Repository};
use crate::sync::missing::MissingSet;
use crate::sync::remote::SyncError;
use crate::tree;

/// Where a ref's tree marks it local-only: `true` here, and no push sends
/// it.
pub const LOCAL_ONLY_PATH: &str = "/_meta/local_only";

/// The filter name the secrets rule reports under.
pub const SECRETS_FILTER: &str = "secrets";

/// Checks a push's new values. `check` sees each value a new commit adds,
/// as text (empty for numbers, booleans and null), with the path it's
/// added at, and returns the names of the rules it breaks.
pub trait PushFilter: Send + Sync {
    /// The name findings are reported under.
    fn name(&self) -> &str;

    fn check(&self, path: &str, value: &str) -> Vec<String>;
}

/// A value a filter flagged. It never carries the value itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// What a [`PushOverride`] names: the same for the same value at the
    /// same path under the same rule, push after push.
    pub id: String,
    pub filter: String,
    pub rule: String,
    pub path: String,
    /// The first new commit that adds it.
    pub commit: ObjectId,
    /// Whether an override can let it through. A secret's can't.
    pub overridable: bool,
}

/// Let one finding through, and say why. The remote records it with the
/// push.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushOverride {
    /// The finding's [`Finding::id`].
    pub finding: String,
    pub reason: String,
}

/// How [`Repository::push_with`] checks what it sends.
#[derive(Clone)]
pub struct PushOptions {
    /// Filters a push's new values go through. [`PushOptions::default`]
    /// has the [`CredentialFilter`]; the secrets and local-only rules apply
    /// whatever this holds.
    pub filters: Vec<Arc<dyn PushFilter>>,
    pub overrides: Vec<PushOverride>,
}

impl Default for PushOptions {
    fn default() -> Self {
        Self {
            filters: vec![Arc::new(CredentialFilter::new())],
            overrides: Vec::new(),
        }
    }
}

impl std::fmt::Debug for PushOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PushOptions")
            .field(
                "filters",
                &self.filters.iter().map(|x| x.name()).collect::<Vec<_>>(),
            )
            .field("overrides", &self.overrides)
            .finish()
    }
}

impl PushOptions {
    /// No filters but the rules that always apply.
    pub fn unfiltered() -> Self {
        Self {
            filters: Vec::new(),
            overrides: Vec::new(),
        }
    }

    pub fn with_filter(mut self, filter: Arc<dyn PushFilter>) -> Self {
        self.filters.push(filter);
        self
    }

    /// Let the finding with this id through, for `reason`.
    pub fn allow(mut self, finding: impl Into<String>, reason: impl Into<String>) -> Self {
        self.overrides.push(PushOverride {
            finding: finding.into(),
            reason: reason.into(),
        });
        self
    }
}

/// Provider token and key shapes, private keys, and password assignments.
/// Each rule is named in its findings; [`CredentialFilter::with_rule`]
/// adds more.
pub struct CredentialFilter {
    names: Vec<String>,
    set: RegexSet,
    /// Keys whose value is a credential however it looks.
    secret_keys: Regex,
}

/// The built-in rules: a name and a pattern each.
const RULES: &[(&str, &str)] = &[
    ("aws-access-key-id", r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b"),
    ("github-token", r"\bgh[pousr]_[A-Za-z0-9]{36,255}\b"),
    (
        "github-fine-grained-token",
        r"\bgithub_pat_[A-Za-z0-9_]{22,255}\b",
    ),
    ("gitlab-token", r"\bglpat-[A-Za-z0-9_\-]{20,}"),
    ("slack-token", r"\bxox[abposr]-[A-Za-z0-9\-]{10,}"),
    ("stripe-live-key", r"\b[rs]k_live_[0-9A-Za-z]{20,}"),
    ("anthropic-api-key", r"\bsk-ant-[A-Za-z0-9_\-]{20,}"),
    ("openai-api-key", r"\bsk-(?:proj-)?[A-Za-z0-9]{20,}"),
    ("google-api-key", r"\bAIza[0-9A-Za-z_\-]{35}"),
    (
        "private-key",
        r"-----BEGIN [A-Z ]*PRIVATE KEY(?: BLOCK)?-----",
    ),
    (
        "password-assignment",
        r#"(?i)\b(?:password|passwd|pwd|secret|api[_-]?key|access[_-]?token|auth[_-]?token|client[_-]?secret)\b["']?\s*[:=]\s*["']?[^\s"']{8,}"#,
    ),
];

/// A value at least this long under a secret-named key is a credential.
const SECRET_VALUE_MIN: usize = 8;

impl CredentialFilter {
    pub fn new() -> Self {
        Self::from_rules(RULES.iter().map(|(n, p)| (n.to_string(), p.to_string())))
            .expect("the built-in rules compile")
    }

    fn from_rules(rules: impl IntoIterator<Item = (String, String)>) -> Result<Self, regex::Error> {
        let (names, patterns): (Vec<String>, Vec<String>) = rules.into_iter().unzip();
        Ok(Self {
            set: RegexSet::new(&patterns)?,
            names,
            secret_keys: Regex::new(
                r"(?i)^(?:password|passwd|secret|api_?key|apikey|access_?token|auth_?token|client_?secret|private_?key)$",
            )?,
        })
    }

    /// The built-in rules plus one more, `pattern` named `name`.
    pub fn with_rule(self, name: &str, pattern: &str) -> Result<Self, regex::Error> {
        let mut rules: Vec<(String, String)> = self
            .names
            .iter()
            .cloned()
            .zip(self.set.patterns().iter().cloned())
            .collect();
        rules.push((name.to_string(), pattern.to_string()));
        Self::from_rules(rules)
    }
}

impl Default for CredentialFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl PushFilter for CredentialFilter {
    fn name(&self) -> &str {
        "credentials"
    }

    fn check(&self, path: &str, value: &str) -> Vec<String> {
        let mut rules: Vec<String> = Vec::new();
        for text in [value, path] {
            for i in self.set.matches(text).iter() {
                if !rules.contains(&self.names[i]) {
                    rules.push(self.names[i].clone());
                }
            }
        }
        let key = path.rsplit('/').next().unwrap_or("");
        if value.chars().count() >= SECRET_VALUE_MIN && self.secret_keys.is_match(key) {
            rules.push("secret-field".to_string());
        }
        rules
    }
}

pub(crate) fn is_secret_path(path: &str) -> bool {
    path == META_SECRET_PREFIX
        || path
            .strip_prefix(META_SECRET_PREFIX)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Whether the tree at `root` marks its ref local-only.
pub(crate) fn tree_is_local_only(store: &dyn Storage, root: &ObjectId) -> Result<bool, RepoError> {
    let path = StatePath::parse(LOCAL_ONLY_PATH)
        .map_err(|e| RepoError::InvalidOperation(e.to_string()))?;
    Ok(matches!(
        tree::tree_get(store, root, &path),
        Ok(Object::Atom(Atom::Bool(true)))
    ))
}

/// Whether the tree at `root` holds anything under `/_meta/_secret`.
pub(crate) fn tree_holds_secrets(store: &dyn Storage, root: &ObjectId) -> Result<bool, RepoError> {
    let path = StatePath::parse(META_SECRET_PREFIX)
        .map_err(|e| RepoError::InvalidOperation(e.to_string()))?;
    Ok(tree::tree_get(store, root, &path).is_ok())
}

/// The atoms the tree at `root` adds over the one at `base`, with their
/// paths. A subtree whose id is unchanged at the same path is skipped
/// whole.
fn new_leaves(
    store: &dyn Storage,
    base: Option<ObjectId>,
    root: ObjectId,
) -> Result<Vec<(String, ObjectId)>, SyncError> {
    let mut out = Vec::new();
    let mut stack = vec![(String::new(), root, base)];
    while let Some((path, id, base)) = stack.pop() {
        if Some(id) == base {
            continue;
        }
        let Some(object) = store.get_object(&id)? else {
            continue;
        };
        let base_node = match base {
            Some(b) => match store.get_object(&b)? {
                Some(Object::Node(node)) => Some(node),
                _ => None,
            },
            None => None,
        };
        match object {
            Object::Atom(_) => out.push((if path.is_empty() { "/".into() } else { path }, id)),
            Object::Node(Node::Map(entries)) => {
                let base_entries = match base_node {
                    Some(Node::Map(m)) => m,
                    _ => Default::default(),
                };
                for (key, child) in entries {
                    let child_base = base_entries.get(&key).copied();
                    stack.push((format!("{path}/{key}"), child, child_base));
                }
            }
            Object::Node(Node::List(items)) | Object::Node(Node::Set(items)) => {
                let base_items = match base_node {
                    Some(Node::List(b)) | Some(Node::Set(b)) => b,
                    _ => Vec::new(),
                };
                for (i, child) in items.into_iter().enumerate() {
                    stack.push((format!("{path}/{i}"), child, base_items.get(i).copied()));
                }
            }
        }
    }
    Ok(out)
}

/// Every finding in what `set` would send. The secrets rule looks at each
/// path a new commit adds; filters look only at values the push sends,
/// since a value the remote already has isn't leaving now.
pub(crate) fn scan(
    store: &dyn Storage,
    set: &MissingSet,
    filters: &[Arc<dyn PushFilter>],
) -> Result<Vec<Finding>, SyncError> {
    let sent: HashSet<ObjectId> = set.objects.iter().copied().collect();
    let mut found: BTreeMap<String, Finding> = BTreeMap::new();
    let mut add = |filter: &str, rule: String, path: &str, atom: &ObjectId, commit: ObjectId| {
        let id = ObjectId::hash(format!("{filter}\0{rule}\0{path}\0{atom}").as_bytes()).short();
        found.entry(id.clone()).or_insert_with(|| Finding {
            id,
            filter: filter.to_string(),
            overridable: filter != SECRETS_FILTER,
            rule,
            path: path.to_string(),
            commit,
        });
    };
    for commit_id in &set.commits {
        let Some(commit) = store.get_commit(commit_id)? else {
            continue;
        };
        if !store.has_object(&commit.state_root)? {
            // Sent without its tree: there's nothing of it to check.
            continue;
        }
        let base = match commit.parents.first() {
            Some(p) => match store.get_commit(p)? {
                Some(c) if store.has_object(&c.state_root)? => Some(c.state_root),
                _ => None,
            },
            None => None,
        };
        for (path, atom) in new_leaves(store, base, commit.state_root)? {
            if is_secret_path(&path) {
                add(
                    SECRETS_FILTER,
                    "meta-secret".into(),
                    &path,
                    &atom,
                    *commit_id,
                );
                continue;
            }
            if filters.is_empty() || !sent.contains(&atom) {
                continue;
            }
            let text = match store.get_object(&atom)? {
                Some(Object::Atom(Atom::String(s))) => s,
                _ => String::new(),
            };
            for filter in filters {
                for rule in filter.check(&path, &text) {
                    add(filter.name(), rule, &path, &atom, *commit_id);
                }
            }
        }
    }
    Ok(found.into_values().collect())
}

/// Split `findings` into those `overrides` let through, each with its
/// override, and those that still block.
pub(crate) fn apply_overrides(
    findings: Vec<Finding>,
    overrides: &[PushOverride],
) -> (Vec<(Finding, PushOverride)>, Vec<Finding>) {
    let mut allowed = Vec::new();
    let mut blocking = Vec::new();
    for finding in findings {
        match overrides
            .iter()
            .find(|o| finding.overridable && o.finding == finding.id)
        {
            Some(o) => allowed.push((finding, o.clone())),
            None => blocking.push(finding),
        }
    }
    (allowed, blocking)
}

/// How a blocked push's error lists its findings, never with their values.
pub(crate) fn describe(findings: &[Finding]) -> String {
    const SHOWN: usize = 5;
    let mut parts: Vec<String> = findings
        .iter()
        .take(SHOWN)
        .map(|f| {
            let fixed = if f.overridable {
                ""
            } else {
                ", can't be overridden"
            };
            format!("{}/{} at {} ({}{fixed})", f.filter, f.rule, f.path, f.id)
        })
        .collect();
    if findings.len() > SHOWN {
        parts.push(format!("and {} more", findings.len() - SHOWN));
    }
    format!(
        "{} finding(s): {}. Remove them, or override each by its id with a reason",
        findings.len(),
        parts.join("; ")
    )
}

impl Repository {
    /// Mark `ref_name` local-only, or clear the mark: a local-only ref is
    /// never pushed, and a remote doesn't offer its own. The mark is
    /// `true` at [`LOCAL_ONLY_PATH`] in the ref's tree, so a branch made
    /// from a marked ref is marked too.
    pub fn set_local_only(&self, ref_name: &str, local_only: bool) -> Result<ObjectId, RepoError> {
        let what = if local_only { "mark" } else { "unmark" };
        let options = CommitOptions::new(
            "system",
            IntentCategory::Migrate,
            format!("{what} {ref_name} local-only"),
        );
        if local_only {
            self.set_json(ref_name, LOCAL_ONLY_PATH, &serde_json::json!(true), options)
        } else if self.is_local_only(ref_name)? {
            self.delete(ref_name, LOCAL_ONLY_PATH, options)
        } else {
            self.head(ref_name)
        }
    }

    /// Whether `ref_name` is marked local-only.
    pub fn is_local_only(&self, ref_name: &str) -> Result<bool, RepoError> {
        let store = self.taint_storage();
        let head = self.head(ref_name)?;
        let commit = store
            .get_commit(&head)?
            .ok_or_else(|| RepoError::CommitNotFound(head.to_string()))?;
        tree_is_local_only(store, &commit.state_root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Credential-shaped text, put together at run time so no source file
    /// holds one.
    fn token(parts: &[&str]) -> String {
        parts.concat()
    }

    fn rules(path: &str, value: &str) -> Vec<String> {
        CredentialFilter::new().check(path, value)
    }

    #[test]
    fn provider_tokens_and_keys_are_found() {
        let cases = [
            (token(&["AK", "IA", &"Q7".repeat(8)]), "aws-access-key-id"),
            (token(&["gh", "p_", &"a1".repeat(18)]), "github-token"),
            (token(&["gl", "pat-", &"x".repeat(20)]), "gitlab-token"),
            (token(&["xo", "xb-", "1234567890-abc"]), "slack-token"),
            (token(&["sk", "_live_", &"9".repeat(24)]), "stripe-live-key"),
            (
                token(&["sk-", "ant-", &"k".repeat(30)]),
                "anthropic-api-key",
            ),
            (token(&["sk-", &"Ab3".repeat(10)]), "openai-api-key"),
            (token(&["AI", "za", &"z".repeat(35)]), "google-api-key"),
            (
                token(&["-----BEGIN ", "OPENSSH ", "PRIVATE KEY-----\nabc"]),
                "private-key",
            ),
            (
                token(&["pass", "word = ", "correct-horse"]),
                "password-assignment",
            ),
        ];
        for (text, rule) in cases {
            let hit = rules("/notes/a", &format!("see {text} here"));
            assert!(hit.contains(&rule.to_string()), "{rule}: {hit:?}");
        }
    }

    #[test]
    fn ordinary_text_passes() {
        for text in [
            "a plain note about passwords in general",
            "sk-short",
            "the AKIA prefix alone",
            "password: no",
        ] {
            assert!(rules("/notes/a", text).is_empty(), "{text:?}");
        }
        // A short value under a secret-named key is fine; a long one isn't.
        assert!(rules("/config/password", "x").is_empty());
        assert_eq!(
            rules("/config/client_secret", "abcdefgh12"),
            vec!["secret-field"]
        );
    }

    #[test]
    fn a_token_in_a_key_is_found() {
        let key = token(&["gh", "p_", &"b2".repeat(18)]);
        assert!(rules(&format!("/tokens/{key}"), "").contains(&"github-token".to_string()));
    }

    #[test]
    fn an_added_rule_applies() {
        let filter = CredentialFilter::new()
            .with_rule("internal-id", r"\bINT-[0-9]{6}\b")
            .unwrap();
        assert_eq!(filter.check("/x", "id INT-123456"), vec!["internal-id"]);
        assert!(
            filter
                .check("/x", &token(&["AK", "IA", &"Q".repeat(16)]))
                .contains(&"aws-access-key-id".to_string())
        );
    }

    #[test]
    fn secret_paths_are_recognised() {
        assert!(is_secret_path("/_meta/_secret"));
        assert!(is_secret_path("/_meta/_secret/key"));
        assert!(!is_secret_path("/_meta/_secrets"));
        assert!(!is_secret_path("/_meta/schema_version"));
    }
}
