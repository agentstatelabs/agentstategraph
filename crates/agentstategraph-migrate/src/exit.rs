//! Exit codes for consumer CLIs that surface `check()` / `run()` results.
//!
//! Values follow `sysexits.h` spirit so ops tooling (systemd, Docker
//! healthchecks) can treat them meaningfully.

/// Everything fine.
pub const OK: i32 = 0;

/// Stored schema is newer than this binary — refusing to start to avoid
/// data loss. Mirrors `EX_USAGE`.
pub const DOWNGRADE_REFUSED: i32 = 64;

/// `/_meta` sentinel is present but unparseable. Mirrors `EX_DATAERR`.
pub const CORRUPT_META: i32 = 65;

/// Upgrade available but the consumer policy is `ASG_MIGRATE=never`.
/// Mirrors `EX_TEMPFAIL` — the condition may be resolved by operator action.
pub const UPGRADE_REQUIRED: i32 = 75;

/// Generic failure running a migration.
pub const MIGRATION_FAILED: i32 = 70;
