//! Adapter bridging `agentstategraph_policy::SignatureVerifier` to the
//! `agentstategraph_policy_sign::Ed25519Verifier` + `canonicalize`
//! primitives.
//!
//! Lives in the MCP crate (not the policy crate) so the policy crate
//! stays free of `ed25519-dalek` / `hex` compile-time cost. §2c of the
//! 0.7.5 plan wires this adapter into `AgentStateGraphServer` via
//! `with_policy_verifier`.

use std::sync::Arc;

use agentstategraph_policy::{
    Policy, PolicySignature, SignatureVerificationError, SignatureVerifier,
};
use agentstategraph_policy_sign::{
    Ed25519Verifier, InMemoryKeyRegistry, KeyRegistry, PolicyVerifier, canonicalize,
};

/// Concrete `SignatureVerifier` that decodes a `PolicySignature::Ed25519`
/// payload, canonicalizes the policy, and delegates to an
/// [`Ed25519Verifier`] backed by the registry `R`.
pub struct Ed25519SignatureVerifier<R: KeyRegistry> {
    inner: Ed25519Verifier<R>,
}

impl<R: KeyRegistry> Ed25519SignatureVerifier<R> {
    /// Wrap an already-populated [`KeyRegistry`].
    pub fn new(registry: R) -> Self {
        Self {
            inner: Ed25519Verifier::new(registry),
        }
    }

    /// Access the underlying Ed25519 verifier (e.g. to inspect the
    /// registry in tests).
    pub fn inner(&self) -> &Ed25519Verifier<R> {
        &self.inner
    }
}

impl<R: KeyRegistry> SignatureVerifier for Ed25519SignatureVerifier<R> {
    fn verify_policy(&self, policy: &Policy) -> Result<(), SignatureVerificationError> {
        let sig = policy
            .signature
            .as_ref()
            .ok_or(SignatureVerificationError::Missing)?;
        match sig {
            PolicySignature::Ed25519 {
                signer_key_id,
                signature_hex,
            } => {
                let sig_bytes =
                    hex::decode(signature_hex).map_err(|_| SignatureVerificationError::Encoding)?;
                let canonical = canonicalize(policy)
                    .map_err(|e| SignatureVerificationError::Invalid(e.to_string()))?;
                self.inner
                    .verify(signer_key_id, &sig_bytes, &canonical)
                    .map_err(|e| SignatureVerificationError::Invalid(e.to_string()))
            }
        }
    }
}

/// Convenience constructor: build an [`InMemoryKeyRegistry`]-backed
/// verifier from a list of `(key_id, verifying_key)` pairs, wrapped in
/// an `Arc` ready to hand to
/// `AgentStateGraphServer::with_policy_verifier`.
pub fn new_in_memory_verifier(
    keys: Vec<(String, ed25519_dalek::VerifyingKey)>,
) -> Arc<Ed25519SignatureVerifier<InMemoryKeyRegistry>> {
    let mut registry = InMemoryKeyRegistry::new();
    for (id, key) in keys {
        registry.insert(id, key);
    }
    Arc::new(Ed25519SignatureVerifier::new(registry))
}
