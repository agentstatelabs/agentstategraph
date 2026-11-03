//! Sync between stores (DESIGN.md Plan G): the pieces push and pull are
//! built from.
//!
//! - [`bundle`]: commits, objects and refs in one compressed blob, with every
//!   id recomputed when it's read (t-002). Store export and import use it
//!   too.
//! - [`missing`]: the commits and objects a remote lacks, from a walk of
//!   both sides' history and a thin diff of the new trees (t-003).

pub mod bundle;
pub mod missing;

pub use bundle::{
    Bundle, BundleError, BundleLimits, BundleReader, BundleRef, BundleWriter, FORMAT_VERSION,
    Record,
};
pub use missing::{MissingSet, missing_set};
