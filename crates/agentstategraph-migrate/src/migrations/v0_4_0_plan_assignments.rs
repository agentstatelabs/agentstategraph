//! Migrate `/plan_assignments` sidecar → native `Task::assigned_to`.
//!
//! Shape of the sidecar, per CTXone's pre-0.4 deployments:
//!
//! ```json
//! {
//!   "plan_assignments": {
//!     "<plan_name>": {
//!       "<task_id>": "<agent_id>"
//!     }
//!   }
//! }
//! ```
//!
//! After migration:
//!  - each referenced task has `assigned_to = Some(agent)`
//!  - the entire `/plan_assignments` subtree is gone
//!  - `/_meta/schema_version` = `"0.4.0"`
//!
//! All four effects land in a single atomic commit tagged
//! `IntentCategory::Migrate`.

use std::sync::LazyLock;

use agentstategraph::{META_SCHEMA_VERSION_PATH, RepoError, Repository};
use semver::{Version, VersionReq};
use serde_json::Value;

use crate::{MigrateError, MigrationOutcome, migrate_commit_options, version_object};

/// The sidecar path CTXone used pre-0.4.
pub const SIDECAR_PATH: &str = "/plan_assignments";

/// Default prefix under which `agentstategraph-tasks` plans are stored.
/// Consumers with a non-standard prefix should register their own
/// `Migration { plans_prefix }` instance.
pub const DEFAULT_PLANS_PREFIX: &str = "/plans";

static FROM_REQ: LazyLock<VersionReq> =
    LazyLock::new(|| VersionReq::parse("<0.4.0").expect("valid range"));
static TO_VER: LazyLock<Version> =
    LazyLock::new(|| Version::parse("0.4.0").expect("valid version"));

/// The migration. Unit struct defaults to `DEFAULT_PLANS_PREFIX`.
pub struct Migration;

impl Migration {
    fn plans_prefix(&self) -> &str {
        DEFAULT_PLANS_PREFIX
    }
}

impl crate::Migration for Migration {
    fn from_version(&self) -> &VersionReq {
        &FROM_REQ
    }

    fn to_version(&self) -> &Version {
        &TO_VER
    }

    fn name(&self) -> &str {
        "plan_assignments_sidecar_to_native"
    }

    fn describe(&self) -> &str {
        "migrate /plan_assignments sidecar → Task.assigned_to"
    }

    fn applies_to(&self, repo: &Repository, ref_name: &str) -> Result<bool, MigrateError> {
        match repo.get_json(ref_name, SIDECAR_PATH) {
            Ok(Value::Object(m)) => Ok(!m.is_empty()),
            Ok(_) => Ok(false),
            Err(RepoError::Tree(_)) => Ok(false),
            Err(e) => Err(MigrateError::Repo(e)),
        }
    }

    fn migrate(&self, repo: &Repository, ref_name: &str) -> Result<MigrationOutcome, MigrateError> {
        let sidecar = match repo.get_json(ref_name, SIDECAR_PATH) {
            Ok(v) => v,
            Err(RepoError::Tree(_)) => {
                // Nothing to migrate — still bump the sentinel in its own commit.
                let commit_id = bump_version_only(repo, ref_name)?;
                return Ok(MigrationOutcome {
                    name: self.name().to_string(),
                    commit_id: Some(commit_id),
                    from: Version::parse("0.3.0").unwrap(),
                    to: TO_VER.clone(),
                    notes: vec!["no sidecar present; stamped schema_version".into()],
                });
            }
            Err(e) => return Err(MigrateError::Repo(e)),
        };

        let plans_map = match sidecar {
            Value::Object(m) => m,
            other => {
                return Err(MigrateError::Corrupt(format!(
                    "{SIDECAR_PATH} must be an object; got {other:?}"
                )));
            }
        };

        let desc = format!("migrate {SIDECAR_PATH} sidecar → Task.assigned_to (→ 0.4.0)");
        let reasoning =
            "native assigned_to supersedes sidecar; spec/UPGRADE-PATH.md §4".to_string();

        let handle = repo.speculate(ref_name, Some(desc.clone()))?;

        let mut touched_tasks: usize = 0;
        let mut notes = Vec::new();

        for (plan_name, per_plan) in plans_map.iter() {
            let Value::Object(task_map) = per_plan else {
                notes.push(format!(
                    "skipping plan {plan_name:?} — value is not an object"
                ));
                continue;
            };

            for (task_id, agent_val) in task_map.iter() {
                let Some(agent) = agent_val.as_str() else {
                    notes.push(format!(
                        "skipping {plan_name}/{task_id} — agent is not a string"
                    ));
                    continue;
                };

                let task_path = format!("{}/{}/{}", self.plans_prefix(), plan_name, task_id);

                let mut task_json = match repo.get_json(ref_name, &task_path) {
                    Ok(v) => v,
                    Err(RepoError::Tree(_)) => {
                        notes.push(format!(
                            "skipping {plan_name}/{task_id} — task missing at {task_path}"
                        ));
                        continue;
                    }
                    Err(e) => {
                        repo.discard_speculation(handle).ok();
                        return Err(MigrateError::Repo(e));
                    }
                };

                let Value::Object(ref mut task_obj) = task_json else {
                    notes.push(format!(
                        "skipping {plan_name}/{task_id} — task JSON is not an object"
                    ));
                    continue;
                };

                task_obj.insert("assigned_to".into(), Value::String(agent.to_string()));

                if let Err(e) = repo.spec_set_json(handle, &task_path, &task_json) {
                    repo.discard_speculation(handle).ok();
                    return Err(MigrateError::Repo(e));
                }
                touched_tasks += 1;
            }
        }

        if let Err(e) = repo.spec_delete(handle, SIDECAR_PATH) {
            repo.discard_speculation(handle).ok();
            return Err(MigrateError::Repo(e));
        }

        let version_val = serde_json::to_value(TO_VER.to_string()).expect("string is valid json");
        if let Err(e) = repo.spec_set_json(handle, META_SCHEMA_VERSION_PATH, &version_val) {
            repo.discard_speculation(handle).ok();
            return Err(MigrateError::Repo(e));
        }

        let opts = migrate_commit_options(self.name(), desc, reasoning);
        let commit_id = repo
            .commit_speculation(handle, opts)
            .map_err(MigrateError::Repo)?;

        notes.insert(0, format!("assigned {touched_tasks} task(s)"));

        Ok(MigrationOutcome {
            name: self.name().to_string(),
            commit_id: Some(commit_id),
            from: Version::parse("0.3.0").unwrap(),
            to: TO_VER.clone(),
            notes,
        })
    }
}

fn bump_version_only(
    repo: &Repository,
    ref_name: &str,
) -> Result<agentstategraph_core::ObjectId, MigrateError> {
    let opts = migrate_commit_options(
        "plan_assignments_sidecar_to_native",
        "stamp schema_version = 0.4.0 (no sidecar present)",
        "no-op migration; advances /_meta/schema_version",
    );
    repo.set(
        ref_name,
        META_SCHEMA_VERSION_PATH,
        &version_object(&TO_VER),
        opts,
    )
    .map_err(MigrateError::Repo)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Migration as MigrationTrait;
    use crate::{CheckResult, Registry, RunMode, check};
    use agentstategraph::{CommitOptions, META_SCHEMA_VERSION_PATH, Repository};
    use agentstategraph_core::IntentCategory;
    use agentstategraph_storage::SqliteStorage;
    use agentstategraph_tasks::{Priority, TaskId, TaskStore};
    use std::sync::Arc;

    /// Emulate a pre-0.4 database: init it, then stomp the schema_version
    /// sentinel to pretend 0.3.0, seed some tasks via TaskStore, and
    /// install a `/plan_assignments` sidecar.
    fn seed_pre_0_4_repo() -> (Arc<Repository>, Vec<(String, TaskId, String)>) {
        let repo = Arc::new(Repository::new(Box::new(SqliteStorage::in_memory().expect("in-memory sqlite"))));
        repo.init().unwrap();

        // Downgrade the stamp using a Migrate commit so the guard allows it.
        repo.set(
            "main",
            META_SCHEMA_VERSION_PATH,
            &agentstategraph_core::Object::Atom(agentstategraph_core::Atom::String("0.3.0".into())),
            CommitOptions::new("test/setup", IntentCategory::Migrate, "seed pre-0.4"),
        )
        .unwrap();

        let store = TaskStore::new(repo.clone(), DEFAULT_PLANS_PREFIX, "seed");

        store.create_plan("main", "alpha", None).unwrap();
        let t1 = store
            .add_task(
                "main",
                "alpha",
                "Write hero",
                Priority::High,
                None,
                vec![],
                None,
            )
            .unwrap();
        let t2 = store
            .add_task(
                "main",
                "alpha",
                "Ship hero",
                Priority::Medium,
                None,
                vec![],
                None,
            )
            .unwrap();

        store.create_plan("main", "beta", None).unwrap();
        let t3 = store
            .add_task(
                "main",
                "beta",
                "Research",
                Priority::Low,
                None,
                vec![],
                None,
            )
            .unwrap();

        let assignments = vec![
            ("alpha".to_string(), t1.id.clone(), "agent-A".to_string()),
            ("alpha".to_string(), t2.id.clone(), "agent-B".to_string()),
            ("beta".to_string(), t3.id.clone(), "agent-A".to_string()),
        ];

        let sidecar = serde_json::json!({
            "alpha": {
                t1.id.as_str(): "agent-A",
                t2.id.as_str(): "agent-B",
            },
            "beta": { t3.id.as_str(): "agent-A" },
        });

        repo.set_json(
            "main",
            SIDECAR_PATH,
            &sidecar,
            CommitOptions::new("test/setup", IntentCategory::Checkpoint, "install sidecar"),
        )
        .unwrap();

        (repo, assignments)
    }

    #[test]
    fn migrates_sidecar_into_tasks_and_bumps_version() {
        let (repo, assignments) = seed_pre_0_4_repo();
        let store = TaskStore::new(repo.clone(), DEFAULT_PLANS_PREFIX, "after");

        let m = Migration;
        assert!(m.applies_to(&repo, "main").unwrap());

        let outcome = m.migrate(&repo, "main").unwrap();
        assert!(outcome.commit_id.is_some());
        assert!(outcome.notes.first().unwrap().contains("assigned 3"));

        for (plan, task_id, agent) in &assignments {
            let task = store.get_task("main", plan, task_id).unwrap();
            assert_eq!(task.assigned_to.as_deref(), Some(agent.as_str()));
        }

        let sidecar = repo.get_json("main", SIDECAR_PATH);
        assert!(sidecar.is_err(), "sidecar should be gone, got {sidecar:?}");

        let v = repo.get_json("main", META_SCHEMA_VERSION_PATH).unwrap();
        assert_eq!(v, serde_json::json!("0.4.0"));
    }

    #[test]
    fn migration_is_idempotent() {
        let (repo, _) = seed_pre_0_4_repo();
        let m = Migration;
        m.migrate(&repo, "main").unwrap();

        assert!(
            !m.applies_to(&repo, "main").unwrap(),
            "should be a no-op now"
        );

        let log_before = repo.log("main", 100).unwrap();
        // Run again — produces one more commit that only bumps version,
        // which is fine (idempotent on data, not on commit count).
        let _ = m.migrate(&repo, "main");
        let log_after = repo.log("main", 100).unwrap();
        // The second run may produce at most one commit (the version stamp).
        assert!(
            log_after.len() - log_before.len() <= 1,
            "expected <=1 additional commit, got {} → {}",
            log_before.len(),
            log_after.len()
        );
    }

    #[test]
    fn migration_commit_is_tagged_migrate() {
        let (repo, _) = seed_pre_0_4_repo();
        Migration.migrate(&repo, "main").unwrap();

        let log = repo.log("main", 10).unwrap();
        let head = &log[0];
        assert_eq!(head.intent.category, IntentCategory::Migrate);
        assert!(head.intent.description.contains("plan_assignments"));
    }

    #[test]
    fn registry_run_end_to_end() {
        let (repo, _) = seed_pre_0_4_repo();
        let registry = Registry::builtin();
        let target = TO_VER.clone();

        let r = check(&repo, "main", &target, &registry).unwrap();
        assert!(
            matches!(r, CheckResult::UpgradeAvailable { .. }),
            "expected UpgradeAvailable, got {r:?}"
        );

        let report = registry
            .run(&repo, "main", &target, RunMode::Apply)
            .unwrap();
        assert_eq!(report.final_version, target);
        assert_eq!(report.steps.len(), 1);

        let r = check(&repo, "main", &target, &registry).unwrap();
        assert!(matches!(r, CheckResult::UpToDate { .. }), "got {r:?}");
    }

    #[test]
    fn dry_run_reports_without_mutating() {
        let (repo, _) = seed_pre_0_4_repo();
        let registry = Registry::builtin();
        let target = TO_VER.clone();

        let log_before = repo.log("main", 100).unwrap().len();
        let report = registry
            .run(&repo, "main", &target, RunMode::DryRun)
            .unwrap();
        let log_after = repo.log("main", 100).unwrap().len();

        assert_eq!(log_before, log_after, "dry-run must not write");
        assert_eq!(report.steps.len(), 1);
        assert!(matches!(
            report.steps[0].status,
            crate::StepStatus::WouldApply
        ));
    }
}
