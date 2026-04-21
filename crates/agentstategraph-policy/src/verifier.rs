//! Signature verifier trait for `PolicyStore`.
//!
//! This trait lives in the main `agentstategraph-policy` crate (rather
//! than in `agentstategraph-policy-sign`) so the policy crate does not
//! need to pull in crypto dependencies to hold the hook. §2c will ship
//! a concrete `Ed25519SignatureVerifier` implementation that composes
//! the `-sign` crate's `Ed25519Verifier` + canonical-JSON helper.
//!
//! Implementations are expected to:
//!
//! 1. Compute the canonical-JSON bytes of the policy with the
//!    `signature` field excluded (via
//!    `agentstategraph_policy_sign::canonicalize` or equivalent).
//! 2. Unpack the policy's `signature` field into
//!    `(signer_key_id, raw_bytes)` and call their verifier.

use thiserror::Error;

use crate::types::Policy;

/// Verify a policy's embedded signature.
///
/// Implementations are expected to compute canonical bytes via the
/// `agentstategraph-policy-sign` crate.
pub trait SignatureVerifier: Send + Sync {
    /// Verify the signature on `policy`. Returns `Ok(())` on a valid
    /// signature; any rejection surfaces as an error.
    fn verify_policy(&self, policy: &Policy) -> Result<(), SignatureVerificationError>;
}

/// Why a signature was rejected.
#[derive(Debug, Error)]
pub enum SignatureVerificationError {
    /// The policy carries no signature at all. Only fatal when the
    /// caller treats unsigned policies as invalid (e.g. server config
    /// `require_signed_policies=true`); the policy crate itself maps
    /// this to "treat as not-currently-active" in that mode.
    #[error("policy has no signature")]
    Missing,

    /// The signature algorithm tag is not supported by this verifier.
    #[error("unsupported algorithm")]
    UnsupportedAlgorithm,

    /// The signature payload (length, hex encoding, key id) is
    /// malformed.
    #[error("invalid signature length or encoding")]
    Encoding,

    /// Cryptographic verification rejected the signature.
    #[error("signature rejected: {0}")]
    Invalid(String),

    /// Any other failure — key-not-found, I/O, canonicalization, etc.
    #[error(transparent)]
    Other(#[from] Box<dyn std::error::Error + Send + Sync>),
}
