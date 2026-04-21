//! Signer trait + Ed25519 default implementation.

use ed25519_dalek::{Signer as _, SigningKey, VerifyingKey};

use crate::error::SignError;

/// Produces an Ed25519 signature over canonical policy bytes.
///
/// The canonical bytes are the output of
/// [`crate::canonical::canonicalize`]. Implementations return the
/// `signer_key_id` they used along with the raw 64-byte signature so
/// the caller can package the pair into a `PolicySignature` (added in
/// §2b of the 0.7.5 plan).
pub trait PolicySigner: Send + Sync {
    /// Sign `canonical` and return `(signer_key_id, signature_bytes)`.
    fn sign(&self, canonical: &[u8]) -> Result<(String, Vec<u8>), SignError>;
}

/// Default Ed25519 signer. Holds a key id (opaque string the verifier
/// uses to look up the corresponding [`VerifyingKey`] in a
/// [`crate::keys::KeyRegistry`]) and a `SigningKey`.
pub struct Ed25519Signer {
    key_id: String,
    signing_key: SigningKey,
}

impl Ed25519Signer {
    /// Construct from an existing [`SigningKey`].
    pub fn new(key_id: impl Into<String>, signing_key: SigningKey) -> Self {
        Self {
            key_id: key_id.into(),
            signing_key,
        }
    }

    /// Construct from a 32-byte seed. Useful for deterministic test
    /// vectors: `Ed25519Signer::from_bytes("test-key", &[1u8; 32])`.
    pub fn from_bytes(key_id: impl Into<String>, bytes: &[u8; 32]) -> Self {
        Self {
            key_id: key_id.into(),
            signing_key: SigningKey::from_bytes(bytes),
        }
    }

    /// Expose the matching verifying (public) key so the caller can
    /// register it with a [`crate::keys::KeyRegistry`].
    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing_key.verifying_key()
    }

    /// Expose the key id.
    pub fn key_id(&self) -> &str {
        &self.key_id
    }
}

impl PolicySigner for Ed25519Signer {
    fn sign(&self, canonical: &[u8]) -> Result<(String, Vec<u8>), SignError> {
        let sig = self.signing_key.sign(canonical);
        Ok((self.key_id.clone(), sig.to_bytes().to_vec()))
    }
}
