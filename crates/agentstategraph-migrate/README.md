# agentstategraph-migrate

Schema migration registry for AgentStateGraph databases.

## What

When the substrate (`agentstategraph`) or a sibling crate
(`agentstategraph-tasks`, future crates) changes how it lays out state,
existing `.db` files need a forward-path. This crate provides:

1. A `/_meta/schema_version` sentinel in-tree (stamped by `Repository::init()`).
2. A `Migration` trait + `Registry` for named, idempotent, commit-producing
   transformations tagged `IntentCategory::Migrate`.
3. A `check()` entry point for consumers to detect mismatched schema
   versions at startup.
4. Stable exit codes (`exit::DOWNGRADE_REFUSED = 64`, etc.) for ops.
5. Builtin migrations — starting with `plan_assignments_sidecar_to_native`
   for CTXone's pre-0.4 sidecar.

See `spec/UPGRADE-PATH.md` in the repo root for the design.

## Consumer integration

Typical startup check, before the app opens any listener:

```rust
use agentstategraph::Repository;
use agentstategraph_migrate::{binary_version, check, exit, CheckResult, Registry, RunMode};

fn boot(repo: &Repository) -> std::process::ExitCode {
    let registry = Registry::builtin();
    let target = binary_version();

    match check(repo, "main", &target, &registry).expect("check failed") {
        CheckResult::UpToDate { .. } => { /* continue */ }

        CheckResult::Unversioned { .. } | CheckResult::UpgradeAvailable { .. } => {
            match std::env::var("ASG_MIGRATE").as_deref() {
                Ok("never") => return std::process::ExitCode::from(exit::UPGRADE_REQUIRED as u8),
                Ok("auto") | Err(_) => {
                    registry.run(repo, "main", &target, RunMode::Apply)
                        .expect("migration failed");
                }
                Ok("prompt") => { /* UI prompt; on decline exit UPGRADE_REQUIRED */ }
                _ => { /* unknown value — treat as auto */ }
            }
        }

        CheckResult::Downgrade { .. } =>
            return std::process::ExitCode::from(exit::DOWNGRADE_REFUSED as u8),
        CheckResult::Corrupt(_) =>
            return std::process::ExitCode::from(exit::CORRUPT_META as u8),
    }

    // ... proceed with normal startup
    std::process::ExitCode::SUCCESS
}
```

For non-interactive deployments (systemd, Docker), set `ASG_MIGRATE=auto`
explicitly so the behaviour is deterministic across versions.

## Operator CLI

Manual / one-shot runs use the subcommand on `agentstategraph-mcp`:

```
agentstategraph-mcp migrate --db ./state.db [--to 0.5.0] [--dry-run] [--yes] [--ref main]
```

The subcommand does **not** start the server. It runs migrations, prints a
report (per-migration status + commit IDs), and exits with codes above.

## Rollback

No `down()` migrations. To roll back:

1. `asg log --intent Migrate` to find the migration commit.
2. Create a branch at its parent: `asg branch pre-0.5 <parent-commit>`.
3. Point the old binary at that branch.

Data on `main` is preserved; the downgrade is a new ref, not a rewrite.

## Registering consumer migrations

Consumers and other sibling crates can register their own migrations:

```rust
let mut registry = Registry::builtin();
registry.register(Box::new(MyAppMigration::new(...)));
```

They appear in `plan()` output ordered by `to_version()`. Each migration
is expected to:

- Be idempotent (`applies_to` returns `false` once done).
- Produce at most one commit tagged `IntentCategory::Migrate`.
- Bump `/_meta/schema_version` to its declared `to_version()` in that
  same commit. Use `spec_set_json` + `commit_speculation` for atomicity.
