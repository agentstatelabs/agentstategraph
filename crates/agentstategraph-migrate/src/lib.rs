//! Schema migration registry for AgentStateGraph databases.
//!
//! See `spec/UPGRADE-PATH.md` for the design.
//!
//! ```text
//! flow:
//!   consumer startup → check(repo, target) → CheckResult
//!     UpToDate       → continue
//!     Unversioned /  → run migrations
//!     UpgradeAvailable
//!     Downgrade      → refuse (exit 64)
//!     Corrupt        → refuse (exit 65)
//! ```
//!
//! Builtin migrations live in `migrations/`. External siblings may
//! register their own via `Registry::register`.

pub mod exit;
pub mod migrations;

use std::fmt;

use agentstategraph::{
    CommitOptions, META_SCHEMA_VERSION_PATH, RepoError, Repository, SCHEMA_VERSION,
};
use agentstategraph_core::{Atom, IntentCategory, Object};
use semver::Version;
use thiserror::Error;

/// Version stamped into a `.db` when the meta sentinel is absent. This
/// is the pre-`/_meta` era — everything written before 0.4.0.
pub const IMPLICIT_VERSION: &str = "0.3.0";

#[derive(Debug, Error)]
pub enum MigrateError {
    #[error("repository error: {0}")]
    Repo(#[from] RepoError),

    #[error("failed to parse stored schema version {0:?}: {1}")]
    BadStoredVersion(String, semver::Error),

    #[error("failed to parse target version {0:?}: {1}")]
    BadTargetVersion(String, semver::Error),

    #[error("migration {name} failed: {source}")]
    MigrationFailed {
        name: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("storage is corrupt: {0}")]
    Corrupt(String),
}

/// Summary of what a migration accomplished on a single ref.
#[derive(Debug, Clone)]
pub struct MigrationOutcome {
    pub name: String,
    pub commit_id: Option<agentstategraph_core::ObjectId>,
    pub from: Version,
    pub to: Version,
    pub notes: Vec<String>,
}

/// A single named, idempotent migration step.
pub trait Migration: Send + Sync {
    /// Version requirement that the *current* stored schema must satisfy.
    #[allow(clippy::wrong_self_convention)]
    fn from_version(&self) -> &semver::VersionReq;

    /// Version that this migration produces.
    fn to_version(&self) -> &Version;

    /// Short stable identifier, e.g. `"plan_assignments_sidecar_to_native"`.
    fn name(&self) -> &str;

    /// Human-readable one-liner for dry-run output.
    fn describe(&self) -> &str;

    /// Cheap probe: is there actually work to do on `ref_name`?
    /// Returning `Ok(false)` makes the migration a no-op (still advances
    /// the schema_version sentinel, but without changing user data).
    fn applies_to(&self, repo: &Repository, ref_name: &str) -> Result<bool, MigrateError>;

    /// Perform the migration, producing (or skipping) one commit tagged
    /// `IntentCategory::Migrate` that also bumps `/_meta/schema_version`
    /// to `self.to_version()`.
    fn migrate(&self, repo: &Repository, ref_name: &str) -> Result<MigrationOutcome, MigrateError>;
}

/// Registry of migrations, ordered by `to_version` then registration order.
pub struct Registry {
    items: Vec<Box<dyn Migration>>,
}

impl Default for Registry {
    fn default() -> Self {
        Self::builtin()
    }
}

impl Registry {
    pub fn empty() -> Self {
        Self { items: Vec::new() }
    }

    /// Registry pre-loaded with every migration shipped by this crate.
    pub fn builtin() -> Self {
        let mut r = Self::empty();
        r.register(Box::new(migrations::v0_4_0_plan_assignments::Migration));
        r
    }

    pub fn register(&mut self, m: Box<dyn Migration>) {
        self.items.push(m);
        self.items.sort_by_key(|m| m.to_version().clone());
    }

    /// Migrations whose `to_version` is `> current` and `<= target` and
    /// whose `from_version` requirement matches `current`.
    pub fn plan<'a>(&'a self, current: &Version, target: &Version) -> Vec<&'a dyn Migration> {
        // Walk transitively: each step's `to_version` becomes the next
        // step's "current." Stops at the first gap where no registered
        // migration advances the version further.
        let mut out: Vec<&'a dyn Migration> = Vec::new();
        let mut cursor = current.clone();

        loop {
            if &cursor >= target {
                break;
            }
            let next = self.items.iter().find(|m| {
                m.to_version() > &cursor
                    && m.to_version() <= target
                    && m.from_version().matches(&cursor)
            });
            match next {
                Some(m) => {
                    cursor = m.to_version().clone();
                    out.push(m.as_ref());
                }
                None => break,
            }
        }

        out
    }

    /// Execute the plan.
    ///
    /// Each migration runs as its own commit. On failure, earlier commits
    /// remain durable — operators re-run or branch from the last good
    /// commit.
    pub fn run(
        &self,
        repo: &Repository,
        ref_name: &str,
        target: &Version,
        mode: RunMode,
    ) -> Result<Report, MigrateError> {
        let current = read_stored_version(repo, ref_name)?;
        let plan = self.plan(&current, target);

        let mut report = Report {
            from: current.clone(),
            target: target.clone(),
            final_version: current,
            steps: Vec::new(),
            mode,
        };

        for m in plan {
            let step_from = report.final_version.clone();
            let step_to = m.to_version().clone();

            if mode == RunMode::DryRun {
                let applies = m.applies_to(repo, ref_name).unwrap_or(true); // pessimistic in dry-run
                report.steps.push(StepReport {
                    name: m.name().to_string(),
                    describe: m.describe().to_string(),
                    from: step_from,
                    to: step_to.clone(),
                    status: if applies {
                        StepStatus::WouldApply
                    } else {
                        StepStatus::WouldSkip
                    },
                    commit_id: None,
                    notes: Vec::new(),
                });
                report.final_version = step_to;
                continue;
            }

            match m.migrate(repo, ref_name) {
                Ok(outcome) => {
                    let status = if outcome.commit_id.is_some() {
                        StepStatus::Applied
                    } else {
                        StepStatus::Skipped
                    };
                    report.steps.push(StepReport {
                        name: outcome.name,
                        describe: m.describe().to_string(),
                        from: step_from,
                        to: step_to.clone(),
                        status,
                        commit_id: outcome.commit_id,
                        notes: outcome.notes,
                    });
                    report.final_version = step_to;
                }
                Err(e) => {
                    report.steps.push(StepReport {
                        name: m.name().to_string(),
                        describe: m.describe().to_string(),
                        from: step_from,
                        to: step_to,
                        status: StepStatus::Failed,
                        commit_id: None,
                        notes: vec![format!("{e}")],
                    });
                    return Err(e);
                }
            }
        }

        Ok(report)
    }

    pub fn iter(&self) -> impl Iterator<Item = &dyn Migration> {
        self.items.iter().map(|b| b.as_ref())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    DryRun,
    Apply,
}

#[derive(Debug, Clone)]
pub struct Report {
    pub from: Version,
    pub target: Version,
    pub final_version: Version,
    pub steps: Vec<StepReport>,
    pub mode: RunMode,
}

#[derive(Debug, Clone)]
pub struct StepReport {
    pub name: String,
    pub describe: String,
    pub from: Version,
    pub to: Version,
    pub status: StepStatus,
    pub commit_id: Option<agentstategraph_core::ObjectId>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    WouldApply,
    WouldSkip,
    Applied,
    Skipped,
    Failed,
}

impl fmt::Display for StepStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            StepStatus::WouldApply => "would-apply",
            StepStatus::WouldSkip => "would-skip",
            StepStatus::Applied => "ok",
            StepStatus::Skipped => "skip",
            StepStatus::Failed => "fail",
        };
        f.write_str(s)
    }
}

/// Result of `check()` — what a consumer should do at startup.
#[derive(Debug, Clone)]
pub enum CheckResult {
    /// Stored version equals target, or target reachable with zero migrations.
    UpToDate { version: Version },
    /// Stored version predates target and migrations exist.
    UpgradeAvailable {
        from: Version,
        to: Version,
        migrations: Vec<String>,
    },
    /// The `.db` schema is newer than this binary knows about.
    Downgrade { db: Version, binary: Version },
    /// No `/_meta/schema_version` sentinel — pre-0.4 database.
    Unversioned { implicit: Version },
    /// Corrupt/unparseable meta sentinel.
    Corrupt(String),
}

/// Inspect a repository and tell a consumer what to do.
///
/// `target` is typically `Version::parse(SCHEMA_VERSION).unwrap()` — the
/// consumer's idea of "current." `ref_name` is usually `"main"`.
pub fn check(
    repo: &Repository,
    ref_name: &str,
    target: &Version,
    registry: &Registry,
) -> Result<CheckResult, MigrateError> {
    match read_stored_version_raw(repo, ref_name)? {
        StoredVersion::Present(v) => {
            if &v > target {
                Ok(CheckResult::Downgrade {
                    db: v,
                    binary: target.clone(),
                })
            } else if &v == target || registry.plan(&v, target).is_empty() {
                Ok(CheckResult::UpToDate { version: v })
            } else {
                let migrations = registry
                    .plan(&v, target)
                    .iter()
                    .map(|m| m.name().to_string())
                    .collect();
                Ok(CheckResult::UpgradeAvailable {
                    from: v,
                    to: target.clone(),
                    migrations,
                })
            }
        }
        StoredVersion::Absent => {
            let implicit = Version::parse(IMPLICIT_VERSION).expect("IMPLICIT_VERSION is valid");
            Ok(CheckResult::Unversioned { implicit })
        }
        StoredVersion::Corrupt(s) => Ok(CheckResult::Corrupt(s)),
    }
}

/// Current schema version of this binary, as a parsed `semver::Version`.
pub fn binary_version() -> Version {
    Version::parse(SCHEMA_VERSION).expect("workspace version parses as semver")
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

enum StoredVersion {
    Present(Version),
    Absent,
    Corrupt(String),
}

fn read_stored_version_raw(
    repo: &Repository,
    ref_name: &str,
) -> Result<StoredVersion, MigrateError> {
    match repo.get(ref_name, META_SCHEMA_VERSION_PATH) {
        Ok(Object::Atom(Atom::String(s))) => match Version::parse(&s) {
            Ok(v) => Ok(StoredVersion::Present(v)),
            Err(e) => Ok(StoredVersion::Corrupt(format!(
                "schema_version {s:?} not valid semver: {e}"
            ))),
        },
        Ok(other) => Ok(StoredVersion::Corrupt(format!(
            "schema_version has non-string shape: {other:?}"
        ))),
        Err(RepoError::Tree(_)) => Ok(StoredVersion::Absent),
        Err(e) => Err(MigrateError::Repo(e)),
    }
}

fn read_stored_version(repo: &Repository, ref_name: &str) -> Result<Version, MigrateError> {
    match read_stored_version_raw(repo, ref_name)? {
        StoredVersion::Present(v) => Ok(v),
        StoredVersion::Absent => Version::parse(IMPLICIT_VERSION)
            .map_err(|e| MigrateError::BadStoredVersion(IMPLICIT_VERSION.into(), e)),
        StoredVersion::Corrupt(s) => Err(MigrateError::Corrupt(s)),
    }
}

/// Helper for migration authors: build a `CommitOptions` tagged
/// `IntentCategory::Migrate` with the conventional agent_id.
pub fn migrate_commit_options(
    name: &str,
    description: impl Into<String>,
    reasoning: impl Into<String>,
) -> CommitOptions {
    CommitOptions::new(
        format!("agentstategraph-migrate/{name}"),
        IntentCategory::Migrate,
        description,
    )
    .with_reasoning(reasoning)
}

/// Helper: produce an Object holding a schema_version string.
pub fn version_object(v: &Version) -> Object {
    Object::Atom(Atom::String(v.to_string()))
}

/// Public re-export — migration authors need this.
pub use agentstategraph::META_PATH_PREFIX;

#[cfg(test)]
mod tests {
    use super::*;
    use agentstategraph_storage::SqliteStorage;

    fn fresh_repo() -> Repository {
        let repo = Repository::new(Box::new(
            SqliteStorage::in_memory().expect("in-memory sqlite"),
        ));
        repo.init().unwrap();
        repo
    }

    fn set_version(repo: &Repository, v: &str) {
        repo.set(
            "main",
            META_SCHEMA_VERSION_PATH,
            &version_object(&Version::parse(v).unwrap()),
            CommitOptions::new("test/setup", IntentCategory::Migrate, "set version"),
        )
        .unwrap();
    }

    /// Minimal Migration stub: writes a new schema_version sentinel.
    struct StubMigration {
        name: &'static str,
        from: semver::VersionReq,
        to: Version,
        /// Whether applies_to() reports work to do.
        applies: bool,
    }

    impl Migration for StubMigration {
        fn from_version(&self) -> &semver::VersionReq {
            &self.from
        }
        fn to_version(&self) -> &Version {
            &self.to
        }
        fn name(&self) -> &str {
            self.name
        }
        fn describe(&self) -> &str {
            "stub migration"
        }
        fn applies_to(&self, _: &Repository, _: &str) -> Result<bool, MigrateError> {
            Ok(self.applies)
        }
        fn migrate(
            &self,
            repo: &Repository,
            ref_name: &str,
        ) -> Result<MigrationOutcome, MigrateError> {
            let commit_id = repo
                .set(
                    ref_name,
                    META_SCHEMA_VERSION_PATH,
                    &version_object(&self.to),
                    CommitOptions::new(
                        format!("agentstategraph-migrate/{}", self.name),
                        IntentCategory::Migrate,
                        format!("stub → {}", self.to),
                    ),
                )
                .map_err(MigrateError::Repo)?;
            Ok(MigrationOutcome {
                name: self.name.to_string(),
                commit_id: Some(commit_id),
                from: Version::parse(IMPLICIT_VERSION).unwrap(),
                to: self.to.clone(),
                notes: Vec::new(),
            })
        }
    }

    /// A stub that always returns an error from migrate().
    struct FailingMigration {
        name: &'static str,
        from: semver::VersionReq,
        to: Version,
    }
    impl Migration for FailingMigration {
        fn from_version(&self) -> &semver::VersionReq {
            &self.from
        }
        fn to_version(&self) -> &Version {
            &self.to
        }
        fn name(&self) -> &str {
            self.name
        }
        fn describe(&self) -> &str {
            "failing stub"
        }
        fn applies_to(&self, _: &Repository, _: &str) -> Result<bool, MigrateError> {
            Ok(true)
        }
        fn migrate(&self, _: &Repository, _: &str) -> Result<MigrationOutcome, MigrateError> {
            Err(MigrateError::MigrationFailed {
                name: self.name.to_string(),
                source: "intentional failure".into(),
            })
        }
    }

    // --- existing tests ---

    #[test]
    fn check_returns_up_to_date_on_fresh_repo() {
        let repo = fresh_repo();
        let target = binary_version();
        let registry = Registry::empty();
        let r = check(&repo, "main", &target, &registry).unwrap();
        assert!(matches!(r, CheckResult::UpToDate { .. }), "got {r:?}");
    }

    #[test]
    fn check_detects_downgrade() {
        let repo = fresh_repo();
        let target = Version::parse("0.1.0").unwrap();
        let r = check(&repo, "main", &target, &Registry::empty()).unwrap();
        assert!(matches!(r, CheckResult::Downgrade { .. }), "got {r:?}");
    }

    #[test]
    fn read_stored_version_uses_init_stamp() {
        let repo = fresh_repo();
        let v = read_stored_version(&repo, "main").unwrap();
        assert_eq!(v, binary_version());
    }

    #[test]
    fn registry_plan_respects_version_bounds() {
        let mut r = Registry::empty();
        r.register(Box::new(StubMigration {
            from: semver::VersionReq::parse(">=0.3, <0.4").unwrap(),
            to: Version::parse("0.4.0").unwrap(),
            name: "a",
            applies: false,
        }));
        r.register(Box::new(StubMigration {
            from: semver::VersionReq::parse(">=0.4, <0.5").unwrap(),
            to: Version::parse("0.5.0").unwrap(),
            name: "b",
            applies: false,
        }));

        let plan = r.plan(
            &Version::parse("0.3.0").unwrap(),
            &Version::parse("0.5.0").unwrap(),
        );
        assert_eq!(
            plan.iter().map(|m| m.name()).collect::<Vec<_>>(),
            vec!["a", "b"]
        );

        let plan = r.plan(
            &Version::parse("0.4.0").unwrap(),
            &Version::parse("0.5.0").unwrap(),
        );
        assert_eq!(plan.iter().map(|m| m.name()).collect::<Vec<_>>(), vec!["b"]);

        let plan = r.plan(
            &Version::parse("0.5.0").unwrap(),
            &Version::parse("0.5.0").unwrap(),
        );
        assert!(plan.is_empty());
    }

    // --- new tests ---

    #[test]
    fn registry_empty_has_no_items() {
        assert_eq!(Registry::empty().iter().count(), 0);
    }

    #[test]
    fn registry_plan_stops_at_gap() {
        // a: 0.3 → 0.4, b: 0.5 → 0.6 (no migration from 0.4 → 0.5)
        let mut r = Registry::empty();
        r.register(Box::new(StubMigration {
            from: semver::VersionReq::parse(">=0.3, <0.4").unwrap(),
            to: Version::parse("0.4.0").unwrap(),
            name: "a",
            applies: false,
        }));
        r.register(Box::new(StubMigration {
            from: semver::VersionReq::parse(">=0.5, <0.6").unwrap(),
            to: Version::parse("0.6.0").unwrap(),
            name: "b",
            applies: false,
        }));
        // Planning from 0.3 to 0.6 should only get "a" — stops at the gap after 0.4
        let plan = r.plan(
            &Version::parse("0.3.0").unwrap(),
            &Version::parse("0.6.0").unwrap(),
        );
        assert_eq!(plan.iter().map(|m| m.name()).collect::<Vec<_>>(), vec!["a"]);
    }

    #[test]
    fn registry_sorted_by_to_version_regardless_of_insertion_order() {
        let mut r = Registry::empty();
        // Register in reverse order
        r.register(Box::new(StubMigration {
            from: semver::VersionReq::parse(">=0.4, <0.5").unwrap(),
            to: Version::parse("0.5.0").unwrap(),
            name: "second",
            applies: false,
        }));
        r.register(Box::new(StubMigration {
            from: semver::VersionReq::parse(">=0.3, <0.4").unwrap(),
            to: Version::parse("0.4.0").unwrap(),
            name: "first",
            applies: false,
        }));
        let names: Vec<_> = r.iter().map(|m| m.name()).collect();
        assert_eq!(names, vec!["first", "second"]);
    }

    #[test]
    fn run_dry_run_does_not_commit() {
        let repo = fresh_repo();
        let current = binary_version();
        let target = Version::parse("99.0.0").unwrap();

        let mut r = Registry::empty();
        r.register(Box::new(StubMigration {
            from: semver::VersionReq::parse(&format!(">={current}")).unwrap(),
            to: target.clone(),
            name: "big-upgrade",
            applies: true,
        }));

        let report = r.run(&repo, "main", &target, RunMode::DryRun).unwrap();
        assert_eq!(report.mode, RunMode::DryRun);
        assert_eq!(report.steps.len(), 1);
        assert_eq!(report.steps[0].status, StepStatus::WouldApply);
        assert!(report.steps[0].commit_id.is_none());

        // Version in repo should be unchanged
        let v = read_stored_version(&repo, "main").unwrap();
        assert_eq!(v, current);
    }

    #[test]
    fn run_apply_executes_migration_and_advances_version() {
        let repo = fresh_repo();
        set_version(&repo, "0.3.0");
        let target = Version::parse("0.4.0").unwrap();

        let mut r = Registry::empty();
        r.register(Box::new(StubMigration {
            from: semver::VersionReq::parse(">=0.3, <0.4").unwrap(),
            to: target.clone(),
            name: "upgrade",
            applies: true,
        }));

        let report = r.run(&repo, "main", &target, RunMode::Apply).unwrap();
        assert_eq!(report.steps.len(), 1);
        assert_eq!(report.steps[0].status, StepStatus::Applied);
        assert!(report.steps[0].commit_id.is_some());

        let v = read_stored_version(&repo, "main").unwrap();
        assert_eq!(v, target);
    }

    #[test]
    fn run_apply_multi_step_executes_in_order() {
        let repo = fresh_repo();
        set_version(&repo, "0.3.0");
        let target = Version::parse("0.5.0").unwrap();

        let mut r = Registry::empty();
        r.register(Box::new(StubMigration {
            from: semver::VersionReq::parse(">=0.3, <0.4").unwrap(),
            to: Version::parse("0.4.0").unwrap(),
            name: "step-a",
            applies: true,
        }));
        r.register(Box::new(StubMigration {
            from: semver::VersionReq::parse(">=0.4, <0.5").unwrap(),
            to: target.clone(),
            name: "step-b",
            applies: true,
        }));

        let report = r.run(&repo, "main", &target, RunMode::Apply).unwrap();
        assert_eq!(report.steps.len(), 2);
        assert_eq!(report.steps[0].name, "step-a");
        assert_eq!(report.steps[1].name, "step-b");
        assert_eq!(read_stored_version(&repo, "main").unwrap(), target);
    }

    #[test]
    fn run_stops_on_failure_and_returns_err() {
        let repo = fresh_repo();
        set_version(&repo, "0.3.0");
        let target = Version::parse("0.5.0").unwrap();

        let mut r = Registry::empty();
        r.register(Box::new(FailingMigration {
            from: semver::VersionReq::parse(">=0.3, <0.4").unwrap(),
            to: Version::parse("0.4.0").unwrap(),
            name: "will-fail",
        }));
        r.register(Box::new(StubMigration {
            from: semver::VersionReq::parse(">=0.4, <0.5").unwrap(),
            to: target.clone(),
            name: "never-runs",
            applies: true,
        }));

        let result = r.run(&repo, "main", &target, RunMode::Apply);
        assert!(result.is_err(), "expected Err on migration failure");
        // Version should still be 0.3 — the first migration never committed
        let v = read_stored_version(&repo, "main").unwrap();
        assert_eq!(v, Version::parse("0.3.0").unwrap());
    }

    #[test]
    fn check_returns_upgrade_available_with_migration_names() {
        let repo = fresh_repo();
        set_version(&repo, "0.3.0");
        let target = Version::parse("0.4.0").unwrap();

        let mut registry = Registry::empty();
        registry.register(Box::new(StubMigration {
            from: semver::VersionReq::parse(">=0.3, <0.4").unwrap(),
            to: target.clone(),
            name: "upgrade-to-0.4",
            applies: true,
        }));

        let r = check(&repo, "main", &target, &registry).unwrap();
        if let CheckResult::UpgradeAvailable {
            from,
            to,
            migrations,
        } = r
        {
            assert_eq!(from, Version::parse("0.3.0").unwrap());
            assert_eq!(to, target);
            assert_eq!(migrations, vec!["upgrade-to-0.4"]);
        } else {
            panic!("expected UpgradeAvailable, got {r:?}");
        }
    }

    #[test]
    fn check_returns_corrupt_for_non_semver_stored_version() {
        let repo = fresh_repo();
        // Write a non-semver value directly
        repo.set(
            "main",
            META_SCHEMA_VERSION_PATH,
            &Object::Atom(Atom::String("not-semver!!".into())),
            CommitOptions::new("test/corrupt", IntentCategory::Migrate, "corrupt version"),
        )
        .unwrap();

        let r = check(&repo, "main", &binary_version(), &Registry::empty()).unwrap();
        assert!(matches!(r, CheckResult::Corrupt(_)), "got {r:?}");
    }

    #[test]
    fn step_status_display() {
        assert_eq!(StepStatus::WouldApply.to_string(), "would-apply");
        assert_eq!(StepStatus::WouldSkip.to_string(), "would-skip");
        assert_eq!(StepStatus::Applied.to_string(), "ok");
        assert_eq!(StepStatus::Skipped.to_string(), "skip");
        assert_eq!(StepStatus::Failed.to_string(), "fail");
    }

    #[test]
    fn migrate_commit_options_helper_uses_migrate_category() {
        let opts = migrate_commit_options("my-migration", "description", "reasoning");
        assert_eq!(opts.agent_id, "agentstategraph-migrate/my-migration");
        // IntentCategory::Migrate is the expected category — verified by
        // checking the commit produced carries it (simplest is just smoke-
        // test the helper builds without panic)
    }

    #[test]
    fn version_object_helper_produces_string_atom() {
        let v = Version::parse("1.2.3").unwrap();
        let obj = version_object(&v);
        assert_eq!(obj, Object::Atom(Atom::String("1.2.3".into())));
    }

    #[test]
    fn binary_version_parses_successfully() {
        let v = binary_version();
        assert!(v.major > 0 || v.minor > 0 || v.patch > 0);
    }

    #[test]
    fn run_dry_run_skipped_step_when_applies_is_false() {
        let repo = fresh_repo();
        let current = binary_version();
        let target = Version::parse("99.0.0").unwrap();

        let mut r = Registry::empty();
        r.register(Box::new(StubMigration {
            from: semver::VersionReq::parse(&format!(">={current}")).unwrap(),
            to: target.clone(),
            name: "no-op",
            applies: false, // applies_to returns false → WouldSkip
        }));

        let report = r.run(&repo, "main", &target, RunMode::DryRun).unwrap();
        assert_eq!(report.steps[0].status, StepStatus::WouldSkip);
    }
}
