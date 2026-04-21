//! Pluggable registry of Ed25519 verifying keys keyed by `signer_key_id`.

use std::collections::HashMap;

use ed25519_dalek::VerifyingKey;

/// Lookup of verifying keys by opaque `signer_key_id`.
pub trait KeyRegistry: Send + Sync {
    /// Return the verifying key for `key_id`, or `None` if unknown.
    fn verifying_key(&self, key_id: &str) -> Option<VerifyingKey>;
}

/// Simple in-memory [`KeyRegistry`]. Suitable for tests and for
/// servers that load keys from disk at startup (D4 / pre-GA key
/// rotation is out of scope per the 0.7.5 plan).
#[derive(Default, Clone)]
pub struct InMemoryKeyRegistry {
    keys: HashMap<String, VerifyingKey>,
}

impl InMemoryKeyRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or overwrite a key. Overwriting is allowed so callers
    /// can implement hot-reload on top of this type.
    pub fn insert(&mut self, key_id: impl Into<String>, key: VerifyingKey) {
        self.keys.insert(key_id.into(), key);
    }

    /// Number of registered keys.
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// `true` if the registry holds no keys.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

impl KeyRegistry for InMemoryKeyRegistry {
    fn verifying_key(&self, key_id: &str) -> Option<VerifyingKey> {
        self.keys.get(key_id).copied()
    }
}
