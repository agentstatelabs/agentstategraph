use agentstategraph::{CommitOptions, META_SCHEMA_VERSION_PATH, Repository};
use agentstategraph_core::{Atom, IntentCategory, Object};
use agentstategraph_migrate::{
    CheckResult, MigrateError, Migration, MigrationOutcome, Registry, RunMode, StepStatus,
    binary_version, check, migrate_commit_options, version_object,
};
use agentstategraph_storage::SqliteStorage;
use semver::{Version, VersionReq};

fn fresh_repo() -> Repository {
    let r = Repository::new(Box::new(SqliteStorage::in_memory().expect("in-memory sqlite")));
    r.init().unwrap();
    r
}

fn set_version(repo: &Repository, v: &str) {
    repo.set(
        "main",
        META_SCHEMA_VERSION_PATH,
        &Object::Atom(Atom::String(v.to_string())),
        CommitOptions::new("test/setup", IntentCategory::Migrate, "set version"),
    )
    .unwrap();
}

// ---------------------------------------------------------------------------
// CheckResult variants
// ---------------------------------------------------------------------------

#[test]
fn check_errors_for_repo_without_init() {
    let repo = Repository::new(Box::new(SqliteStorage::in_memory().expect("in-memory sqlite")));
    // No init → no branch → get() returns RepoError::Branch → MigrateError::Repo
    let target = binary_version();
    let r = check(&repo, "main", &target, &Registry::empty());
    assert!(
        r.is_err(),
        "check() on an uninitted repo should return an error; got {r:?}"
    );
}

#[test]
fn check_corrupt_for_invalid_semver_sentinel() {
    let repo = fresh_repo();
    set_version(&repo, "not-a-semver-string!!!");
    let target = binary_version();
    let r = check(&repo, "main", &target, &Registry::empty()).unwrap();
    assert!(
        matches!(r, CheckResult::Corrupt(_)),
        "expected Corrupt, got {r:?}"
    );
}

#[test]
fn check_upgrade_available_with_stub_migration() {
    let repo = fresh_repo();
    // Downgrade the stored version to 0.3.0 so there's something to migrate.
    set_version(&repo, "0.3.0");

    let target = Version::parse("0.4.0").unwrap();
    let registry = {
        let mut r = Registry::empty();
        r.register(Box::new(StubMigration::new("step-a", ">=0.3, <0.4", "0.4.0")));
        r
    };

    let r = check(&repo, "main", &target, &registry).unwrap();
    match r {
        CheckResult::UpgradeAvailable { from, to, migrations } => {
            assert_eq!(from, Version::parse("0.3.0").unwrap());
            assert_eq!(to, target);
            assert_eq!(migrations, vec!["step-a"]);
        }
        other => panic!("expected UpgradeAvailable, got {other:?}"),
    }
}

#[test]
fn check_up_to_date_after_migration() {
    let repo = fresh_repo();
    set_version(&repo, "0.3.0");

    let target = Version::parse("0.4.0").unwrap();
    let mut registry = Registry::empty();
    registry.register(Box::new(StubMigration::new("step-a", ">=0.3, <0.4", "0.4.0")));

    registry
        .run(&repo, "main", &target, RunMode::Apply)
        .unwrap();

    let r = check(&repo, "main", &target, &registry).unwrap();
    assert!(
        matches!(r, CheckResult::UpToDate { .. }),
        "expected UpToDate after migration, got {r:?}"
    );
}

// ---------------------------------------------------------------------------
// Registry::plan — gap detection
// ---------------------------------------------------------------------------

#[test]
fn plan_stops_at_version_gap() {
    // Registry has A→B and C→D but nothing for B→C.
    // Plan from A to D should only produce [A→B] then stop.
    let mut registry = Registry::empty();
    registry.register(Box::new(StubMigration::new("a-to-b", ">=0.1, <0.2", "0.2.0")));
    registry.register(Box::new(StubMigration::new("c-to-d", ">=0.3, <0.4", "0.4.0")));

    let plan = registry.plan(
        &Version::parse("0.1.0").unwrap(),
        &Version::parse("0.4.0").unwrap(),
    );
    assert_eq!(plan.len(), 1, "plan should stop at the gap");
    assert_eq!(plan[0].name(), "a-to-b");
}

#[test]
fn plan_empty_when_current_equals_target() {
    let registry = {
        let mut r = Registry::empty();
        r.register(Box::new(StubMigration::new("s", ">=0.1, <0.2", "0.2.0")));
        r
    };
    let v = Version::parse("0.2.0").unwrap();
    let plan = registry.plan(&v, &v);
    assert!(plan.is_empty());
}

#[test]
fn plan_empty_when_no_applicable_migration() {
    let registry = {
        let mut r = Registry::empty();
        r.register(Box::new(StubMigration::new("s", ">=0.3, <0.4", "0.4.0")));
        r
    };
    // Current is 0.1.0 — migration requires >=0.3, so no match.
    let plan = registry.plan(
        &Version::parse("0.1.0").unwrap(),
        &Version::parse("0.4.0").unwrap(),
    );
    assert!(plan.is_empty(), "no migration covers 0.1→0.4");
}

// ---------------------------------------------------------------------------
// Multi-step registry run
// ---------------------------------------------------------------------------

#[test]
fn multi_step_run_executes_in_version_order() {
    let repo = fresh_repo();
    set_version(&repo, "0.3.0");

    let mut registry = Registry::empty();
    // Register out-of-order to verify sorting.
    registry.register(Box::new(StubMigration::new("a-to-b", ">=0.3, <0.4", "0.4.0")));
    registry.register(Box::new(StubMigration::new("b-to-c", ">=0.4, <0.5", "0.5.0")));

    let target = Version::parse("0.5.0").unwrap();
    let report = registry
        .run(&repo, "main", &target, RunMode::Apply)
        .unwrap();

    assert_eq!(report.steps.len(), 2);
    assert_eq!(report.steps[0].name, "a-to-b");
    assert_eq!(report.steps[1].name, "b-to-c");
    assert_eq!(
        report.steps[0].status,
        StepStatus::Applied,
        "first step should be Applied"
    );
    assert_eq!(
        report.steps[1].status,
        StepStatus::Applied,
        "second step should be Applied"
    );
    assert_eq!(report.final_version, target);
}

#[test]
fn multi_step_dry_run_does_not_mutate() {
    let repo = fresh_repo();
    set_version(&repo, "0.3.0");

    let mut registry = Registry::empty();
    registry.register(Box::new(StubMigration::new("a-to-b", ">=0.3, <0.4", "0.4.0")));
    registry.register(Box::new(StubMigration::new("b-to-c", ">=0.4, <0.5", "0.5.0")));

    let log_before = repo.log("main", 100).unwrap().len();
    let report = registry
        .run(
            &repo,
            "main",
            &Version::parse("0.5.0").unwrap(),
            RunMode::DryRun,
        )
        .unwrap();
    let log_after = repo.log("main", 100).unwrap().len();

    assert_eq!(log_before, log_after, "DryRun must not write commits");
    assert_eq!(report.steps.len(), 2);
    assert!(report
        .steps
        .iter()
        .all(|s| s.status == StepStatus::WouldApply));
    // DryRun still advances final_version in the report.
    assert_eq!(report.final_version, Version::parse("0.5.0").unwrap());
}

// ---------------------------------------------------------------------------
// Skipped step (applies_to returns false)
// ---------------------------------------------------------------------------

#[test]
fn applied_step_when_applies_to_true() {
    let repo = fresh_repo();
    set_version(&repo, "0.3.0");

    let mut registry = Registry::empty();
    registry.register(Box::new(StubMigration::applies(
        "s",
        ">=0.3, <0.4",
        "0.4.0",
        true,
    )));

    let report = registry
        .run(
            &repo,
            "main",
            &Version::parse("0.4.0").unwrap(),
            RunMode::Apply,
        )
        .unwrap();

    assert_eq!(report.steps[0].status, StepStatus::Applied);
    assert!(report.steps[0].commit_id.is_some());
}

#[test]
fn skipped_step_when_applies_to_false() {
    let repo = fresh_repo();
    set_version(&repo, "0.3.0");

    let mut registry = Registry::empty();
    registry.register(Box::new(StubMigration::applies(
        "s",
        ">=0.3, <0.4",
        "0.4.0",
        false,
    )));

    let report = registry
        .run(
            &repo,
            "main",
            &Version::parse("0.4.0").unwrap(),
            RunMode::Apply,
        )
        .unwrap();

    assert_eq!(report.steps[0].status, StepStatus::Skipped);
    assert!(report.steps[0].commit_id.is_none());
}

#[test]
fn dry_run_would_skip_when_applies_to_false() {
    let repo = fresh_repo();
    set_version(&repo, "0.3.0");

    let mut registry = Registry::empty();
    registry.register(Box::new(StubMigration::applies(
        "s",
        ">=0.3, <0.4",
        "0.4.0",
        false,
    )));

    let report = registry
        .run(
            &repo,
            "main",
            &Version::parse("0.4.0").unwrap(),
            RunMode::DryRun,
        )
        .unwrap();

    assert_eq!(report.steps[0].status, StepStatus::WouldSkip);
}

// ---------------------------------------------------------------------------
// Registry::iter ordering
// ---------------------------------------------------------------------------

#[test]
fn registry_iter_sorted_by_to_version() {
    let mut registry = Registry::empty();
    // Register in reverse order.
    registry.register(Box::new(StubMigration::new("c", ">=0.5, <0.6", "0.6.0")));
    registry.register(Box::new(StubMigration::new("a", ">=0.3, <0.4", "0.4.0")));
    registry.register(Box::new(StubMigration::new("b", ">=0.4, <0.5", "0.5.0")));

    let names: Vec<_> = registry.iter().map(|m| m.name()).collect();
    assert_eq!(names, vec!["a", "b", "c"]);
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

#[test]
fn binary_version_matches_schema_version_constant() {
    let v = binary_version();
    let expected = agentstategraph::SCHEMA_VERSION;
    assert_eq!(v.to_string(), expected);
}

#[test]
fn version_object_is_string_atom() {
    let v = Version::parse("1.2.3").unwrap();
    let obj = version_object(&v);
    assert_eq!(obj, Object::Atom(Atom::String("1.2.3".to_string())));
}

#[test]
fn migrate_commit_options_has_migrate_intent() {
    let opts = migrate_commit_options("my-migration", "do the thing", "because reasons");
    assert_eq!(opts.intent.category, IntentCategory::Migrate);
    assert!(
        opts.agent_id.contains("agentstategraph-migrate"),
        "agent_id should identify the migrate crate: {}",
        opts.agent_id
    );
    assert!(opts.intent.description.contains("do the thing"));
}

// ---------------------------------------------------------------------------
// Report field accuracy
// ---------------------------------------------------------------------------

#[test]
fn report_from_and_target_fields() {
    let repo = fresh_repo();
    set_version(&repo, "0.3.0");

    let mut registry = Registry::empty();
    registry.register(Box::new(StubMigration::new("s", ">=0.3, <0.4", "0.4.0")));

    let target = Version::parse("0.4.0").unwrap();
    let report = registry
        .run(&repo, "main", &target, RunMode::Apply)
        .unwrap();

    assert_eq!(report.from, Version::parse("0.3.0").unwrap());
    assert_eq!(report.target, target);
    assert_eq!(report.final_version, target);
    assert_eq!(report.mode, RunMode::Apply);
}

#[test]
fn step_report_from_to_match_migration_versions() {
    let repo = fresh_repo();
    set_version(&repo, "0.3.0");

    let mut registry = Registry::empty();
    registry.register(Box::new(StubMigration::new("s", ">=0.3, <0.4", "0.4.0")));

    let report = registry
        .run(
            &repo,
            "main",
            &Version::parse("0.4.0").unwrap(),
            RunMode::Apply,
        )
        .unwrap();

    let step = &report.steps[0];
    assert_eq!(step.from, Version::parse("0.3.0").unwrap());
    assert_eq!(step.to, Version::parse("0.4.0").unwrap());
    assert_eq!(step.name, "s");
}

// ---------------------------------------------------------------------------
// Stub migration — reusable test double
// ---------------------------------------------------------------------------

struct StubMigration {
    name: &'static str,
    from: VersionReq,
    to: Version,
    /// If false, applies_to returns false → migrate() no-ops (no commit).
    applies: bool,
}

impl StubMigration {
    fn new(name: &'static str, from: &str, to: &str) -> Self {
        Self {
            name,
            from: VersionReq::parse(from).unwrap(),
            to: Version::parse(to).unwrap(),
            applies: true,
        }
    }

    fn applies(name: &'static str, from: &str, to: &str, applies: bool) -> Self {
        Self {
            name,
            from: VersionReq::parse(from).unwrap(),
            to: Version::parse(to).unwrap(),
            applies,
        }
    }
}

impl Migration for StubMigration {
    fn from_version(&self) -> &VersionReq {
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
    fn applies_to(&self, _repo: &Repository, _ref_name: &str) -> Result<bool, MigrateError> {
        Ok(self.applies)
    }
    fn migrate(&self, repo: &Repository, ref_name: &str) -> Result<MigrationOutcome, MigrateError> {
        if !self.applies {
            return Ok(MigrationOutcome {
                name: self.name.to_string(),
                commit_id: None,
                from: self.from.to_string().parse().unwrap_or(Version::new(0, 0, 0)),
                to: self.to.clone(),
                notes: vec!["skipped".into()],
            });
        }
        let commit_id = repo
            .set(
                ref_name,
                META_SCHEMA_VERSION_PATH,
                &version_object(&self.to),
                CommitOptions::new(
                    format!("agentstategraph-migrate/{}", self.name),
                    IntentCategory::Migrate,
                    format!("stub migration → {}", self.to),
                ),
            )
            .map_err(MigrateError::Repo)?;
        Ok(MigrationOutcome {
            name: self.name.to_string(),
            commit_id: Some(commit_id),
            from: self.from.to_string().parse().unwrap_or(Version::new(0, 0, 0)),
            to: self.to.clone(),
            notes: Vec::new(),
        })
    }
}
