//! `agentstategraph-mcp migrate` — one-shot schema migration runner.
//!
//! Not a server mode. Refuses to start the MCP/HTTP surface; operator
//! runs this against a `.db` file, reports, and exits.

use std::sync::Arc;

use agentstategraph::Repository;
use agentstategraph_migrate::{
    CheckResult, Registry, RunMode, binary_version, check, exit as exit_codes,
};
use agentstategraph_storage::SqliteStorage;
use semver::Version;

pub fn run(args: &[String]) -> i32 {
    let mut db_path = "./agentstategraph.db".to_string();
    let mut storage_type = "sqlite";
    let mut ref_name = "main".to_string();
    let mut target_override: Option<String> = None;
    let mut dry_run = false;
    let mut yes = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--db" | "--path" | "-p" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    db_path = v.clone();
                }
            }
            "--storage" | "-s" => {
                i += 1;
                match args.get(i).map(String::as_str) {
                    Some("memory") => storage_type = "memory",
                    Some("sqlite") | Some(_) => storage_type = "sqlite",
                    None => {}
                }
            }
            "--to" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    target_override = Some(v.clone());
                }
            }
            "--ref" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    ref_name = v.clone();
                }
            }
            "--dry-run" => dry_run = true,
            "--yes" | "-y" => yes = true,
            "--help" | "-h" => {
                print_help();
                return exit_codes::OK;
            }
            other => {
                eprintln!("migrate: unknown argument {other:?}");
                print_help();
                return 2;
            }
        }
        i += 1;
    }

    let target = match target_override {
        Some(s) => match Version::parse(&s) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("migrate: --to {s:?} is not valid semver: {e}");
                return 2;
            }
        },
        None => binary_version(),
    };

    let repo: Arc<Repository> = match storage_type {
        "memory" => Arc::new(Repository::new(Box::new(
            SqliteStorage::in_memory().expect("in-memory sqlite"),
        ))),
        _ => match SqliteStorage::open(&db_path) {
            Ok(s) => Arc::new(Repository::new(Box::new(s))),
            Err(e) => {
                eprintln!("migrate: failed to open {db_path:?}: {e}");
                return exit_codes::MIGRATION_FAILED;
            }
        },
    };

    if let Err(e) = repo.init() {
        eprintln!("migrate: init failed: {e}");
        return exit_codes::MIGRATION_FAILED;
    }

    let registry = Registry::builtin();

    match check(&repo, &ref_name, &target, &registry) {
        Ok(CheckResult::UpToDate { version }) => {
            eprintln!("up-to-date: schema_version={version} (target {target}); nothing to do");
            return exit_codes::OK;
        }
        Ok(CheckResult::Downgrade { db, binary }) => {
            eprintln!("refusing to downgrade: db schema {db} is newer than binary {binary}");
            return exit_codes::DOWNGRADE_REFUSED;
        }
        Ok(CheckResult::Corrupt(msg)) => {
            eprintln!("corrupt /_meta: {msg}");
            return exit_codes::CORRUPT_META;
        }
        Ok(CheckResult::UpgradeAvailable {
            from,
            to,
            ref migrations,
        }) => {
            eprintln!("plan: {from} → {to} via {} migration(s):", migrations.len());
            for name in migrations {
                eprintln!("  - {name}");
            }
        }
        Ok(CheckResult::Unversioned { implicit }) => {
            eprintln!("unversioned db (implicit {implicit}); target {target}");
        }
        Err(e) => {
            eprintln!("migrate: check failed: {e}");
            return exit_codes::MIGRATION_FAILED;
        }
    }

    if dry_run {
        match registry.run(&repo, &ref_name, &target, RunMode::DryRun) {
            Ok(report) => {
                print_report(&report);
                return exit_codes::OK;
            }
            Err(e) => {
                eprintln!("migrate: dry-run failed: {e}");
                return exit_codes::MIGRATION_FAILED;
            }
        }
    }

    if !yes && !prompt_confirm() {
        eprintln!("aborted.");
        return exit_codes::OK;
    }

    match registry.run(&repo, &ref_name, &target, RunMode::Apply) {
        Ok(report) => {
            print_report(&report);
            exit_codes::OK
        }
        Err(e) => {
            eprintln!("migrate: failed: {e}");
            exit_codes::MIGRATION_FAILED
        }
    }
}

fn prompt_confirm() -> bool {
    use std::io::{BufRead, Write};
    eprint!("Apply? [y/N] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim(), "y" | "Y" | "yes" | "YES")
}

fn print_report(report: &agentstategraph_migrate::Report) {
    let mode = match report.mode {
        RunMode::DryRun => "dry-run",
        RunMode::Apply => "apply",
    };
    eprintln!("--- {mode} report ---");
    eprintln!("from {} → final {}", report.from, report.final_version);
    for step in &report.steps {
        let commit = step
            .commit_id
            .as_ref()
            .map(|id| format!(" commit={id}"))
            .unwrap_or_default();
        eprintln!(
            "  [{status}] {name} ({from} → {to}){commit}",
            status = step.status,
            name = step.name,
            from = step.from,
            to = step.to,
        );
        for note in &step.notes {
            eprintln!("      · {note}");
        }
    }
}

fn print_help() {
    eprintln!("usage: agentstategraph-mcp migrate [OPTIONS]");
    eprintln!();
    eprintln!("OPTIONS:");
    eprintln!("  --db, --path, -p <PATH>  SQLite database path (default: ./agentstategraph.db)");
    eprintln!("  --storage, -s <TYPE>     sqlite (default) or memory");
    eprintln!("  --ref <NAME>             Ref to migrate (default: main)");
    eprintln!("  --to <VERSION>           Target schema version (default: binary version)");
    eprintln!("  --dry-run                Print plan without applying");
    eprintln!("  --yes, -y                Apply without prompting");
    eprintln!("  -h, --help               Print help");
    eprintln!();
    eprintln!("EXIT CODES:");
    eprintln!("   0  ok");
    eprintln!("  64  refused: db schema newer than binary");
    eprintln!("  65  /_meta corrupt");
    eprintln!("  70  migration failed");
    eprintln!("  75  upgrade required but policy=never");
}
