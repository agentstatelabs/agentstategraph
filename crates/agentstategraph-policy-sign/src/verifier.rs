//! Verifier trait + Ed25519 default implementation.

use ed25519_dalek::Signature;

use crate::error::VerifyError;
use crate::keys::KeyRegistry;

/// Checks an Ed25519 signature against canonical policy bytes.
///
/// The verifier is deliberately decoupled from the `PolicySignature`
/// enum (which §2b adds to `agentstategraph-policy::types`). Callers
/// unpack the enum into `(signer_key_id, signature_bytes)` before
/// invoking this trait.
pub trait PolicyVerifier: Send + Sync {
    /// Verify that `signature` (64 raw bytes) is valid over
    /// `canonical` under the key registered at `signer_key_id`.
    fn verify(
        &self,
        signer_key_id: &str,
        signature: &[u8],
        canonical: &[u8],
    ) -> Result<(), VerifyError>;
}

/// Default verifier — looks up a [`VerifyingKey`] in the registry and
/// runs `ed25519_dalek::VerifyingKey::verify_strict`.
pub struct Ed25519Verifier<R: KeyRegistry> {
    registry: R,
}

impl<R: KeyRegistry> Ed25519Verifier<R> {
    pub fn new(registry: R) -> Self {
        Self { registry }
    }

    /// Access the underlying registry.
    pub fn registry(&self) -> &R {
        &self.registry
    }

    /// Mutable access to the underlying registry (e.g. to insert a
    /// key after construction).
    pub fn registry_mut(&mut self) -> &mut R {
        &mut self.registry
    }
}

impl<R: KeyRegistry> PolicyVerifier for Ed25519Verifier<R> {
    fn verify(
        &self,
        signer_key_id: &str,
        signature: &[u8],
        canonical: &[u8],
    ) -> Result<(), VerifyError> {
        let key = self
            .registry
            .verifying_key(signer_key_id)
            .ok_or_else(|| VerifyError::KeyNotFound(signer_key_id.to_string()))?;
        let sig_bytes: [u8; 64] = signature
            .try_into()
            .map_err(|_| VerifyError::InvalidSignatureLength)?;
        let sig = Signature::from_bytes(&sig_bytes);
        key.verify_strict(canonical, &sig)
            .map_err(|_| VerifyError::Invalid)?;
        Ok(())
    }
}
