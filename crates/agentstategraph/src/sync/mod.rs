//! Sync between stores (DESIGN.md Plan G): the pieces push and pull are
//! built from.
//!
//! - [`bundle`]: commits, objects and refs in one compressed blob, with every
//!   id recomputed when it's read (t-002). Store export and import use it
//!   too.
//! - [`missing`]: the commits and objects a remote lacks, from a walk of
//!   both sides' history and a thin diff of the new trees (t-003).
//! - [`store`]: a whole store exported to a bundle and imported from one
//!   (Plan F t-008).
//! - [`remote`]: the [`Remote`] trait, and push, fetch and pull through
//!   one, with remote-tracking refs (t-004).
//! - [`filter`]: what a push refuses to send: secrets, local-only refs, and
//!   whatever its [`PushFilter`]s find, such as credentials (t-011, t-005).
//! - [`push_log`]: the receiving side's record of the pushes it took (t-006).

pub mod bundle;
pub mod filter;
pub mod missing;
pub mod push_log;
pub mod remote;
pub mod store;

pub use bundle::{
    Bundle, BundleError, BundleLimits, BundleReader, BundleRef, BundleWriter, FORMAT_VERSION,
    Record,
};
pub use filter::{
    CredentialFilter, Finding, LOCAL_ONLY_PATH, PushFilter, PushOptions, PushOverride,
};
pub use missing::{MissingSet, missing_set};
pub use push_log::{PUSH_LOG_REF, PushLogEntry};
pub use remote::{
    FetchReport, FetchRequest, InProcessRemote, PullOutcome, PullReport, PushOutcome, PushReport,
    RefChange, RefUpdate, Remote, SyncError, tracking_ref,
};
pub use store::{ExportReport, ImportReport, TransferError};
