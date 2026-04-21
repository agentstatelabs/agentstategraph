//! Error types for signing and verification.

use thiserror::Error;

/// Errors produced by the signing pipeline.
#[derive(Debug, Error)]
pub enum SignError {
    /// Serializing the policy to canonical JSON failed.
    #[error("canonicalize failed: {0}")]
    CanonicalizeFailed(String),
    /// The underlying signer rejected the bytes or produced an error.
    #[error("signing failed: {0}")]
    SigningFailed(String),
}

/// Errors produced by the verification pipeline.
#[derive(Debug, Error)]
pub enum VerifyError {
    /// `signer_key_id` was not present in the registry.
    #[error("signer key not found: {0}")]
    KeyNotFound(String),
    /// Signature bytes were not 64 bytes long.
    #[error("invalid signature length (expected 64 bytes)")]
    InvalidSignatureLength,
    /// The signature did not verify against the canonical bytes.
    #[error("signature verification failed")]
    Invalid,
}
