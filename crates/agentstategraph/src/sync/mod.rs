//! Sync between stores (DESIGN.md Plan G): the pieces push and pull are
//! built from.
//!
//! - [`bundle`]: commits, objects and refs in one compressed blob, with every
//!   id recomputed when it's read (t-002). Store export and import use it
//!   too.

pub mod bundle;

pub use bundle::{
    Bundle, BundleError, BundleLimits, BundleReader, BundleRef, BundleWriter, FORMAT_VERSION,
    Record,
};
