//! Integration tests for `agentstategraph-policy-sign`.

use agentstategraph_policy::selector::Selector;
use agentstategraph_policy::types::Policy;
use agentstategraph_policy_sign::{
    Ed25519Signer, Ed25519Verifier, InMemoryKeyRegistry, PolicySigner, PolicyVerifier, VerifyError,
    canonicalize, canonicalize_value,
};
use chrono::{TimeZone, Utc};
use ed25519_dalek::SigningKey;
use serde_json::json;

fn sample_policy() -> Policy {
    Policy {
        path: "infra/k8s/pod-failing".into(),
        version: 1,
        situation: "pod keeps crashing".into(),
        situation_selector: Selector::Always,
        allow: vec![],
        deny: vec![],
        require_approval: vec![],
        procedure: None,
        triggers: vec![],
        required_fields: vec![],
        severity: Default::default(),
        proposed_by: "alice".into(),
        proposed_at: Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap(),
        ratified_by: Some("bob".into()),
        ratified_at: Some(Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 6).unwrap()),
        ratification_reasoning: None,
        active_from: Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 7).unwrap(),
        expires_at: None,
        supersedes: None,
        signature: None,
        tenant_id: None,
        external_evaluator: None,
    }
}

/// Deterministic signer for tests: seed = [1u8; 32].
fn deterministic_signer(key_id: &str) -> Ed25519Signer {
    Ed25519Signer::from_bytes(key_id, &[1u8; 32])
}

#[test]
fn canonicalize_produces_sorted_keys() {
    let v = json!({ "z": 1, "a": 2, "m": 3 });
    let bytes = canonicalize_value(v).unwrap();
    let s = std::str::from_utf8(&bytes).unwrap();
    assert_eq!(s, r#"{"a":2,"m":3,"z":1}"#);
}

#[test]
fn canonicalize_is_deterministic() {
    let a = json!({
        "path": "x",
        "version": 2,
        "nested": { "b": [3, 1, 2], "a": true },
    });
    let b = json!({
        "nested": { "a": true, "b": [3, 1, 2] },
        "version": 2,
        "path": "x",
    });
    assert_eq!(
        canonicalize_value(a).unwrap(),
        canonicalize_value(b).unwrap()
    );
}

#[test]
fn canonicalize_excludes_signature_field() {
    // Forward-compat guard for §2b: even once `Policy.signature` lands,
    // it must not appear in the canonical bytes.
    let v = json!({
        "path": "x",
        "version": 1,
        "signature": { "algorithm": "ed25519", "signer_key_id": "k1", "signature_hex": "abcd" },
    });
    let bytes = canonicalize_value(v).unwrap();
    let s = std::str::from_utf8(&bytes).unwrap();
    assert!(!s.contains("signature"), "got: {s}");
    assert!(!s.contains("ed25519"), "got: {s}");
}

#[test]
fn sign_and_verify_round_trip() {
    let signer = deterministic_signer("test-key");
    let mut registry = InMemoryKeyRegistry::new();
    registry.insert("test-key", signer.verifying_key());
    let verifier = Ed25519Verifier::new(registry);

    let policy = sample_policy();
    let bytes = canonicalize(&policy).unwrap();
    let (key_id, sig) = signer.sign(&bytes).unwrap();
    assert_eq!(key_id, "test-key");
    assert_eq!(sig.len(), 64);
    verifier.verify(&key_id, &sig, &bytes).expect("must verify");
}

#[test]
fn verify_rejects_wrong_signer_id() {
    let signer = deterministic_signer("test-key");
    let mut registry = InMemoryKeyRegistry::new();
    registry.insert("test-key", signer.verifying_key());
    let verifier = Ed25519Verifier::new(registry);

    let policy = sample_policy();
    let bytes = canonicalize(&policy).unwrap();
    let (_k, sig) = signer.sign(&bytes).unwrap();

    match verifier.verify("unknown-key", &sig, &bytes) {
        Err(VerifyError::KeyNotFound(k)) => assert_eq!(k, "unknown-key"),
        other => panic!("expected KeyNotFound, got {other:?}"),
    }
}

#[test]
fn verify_rejects_tampered_canonical_bytes() {
    let signer = deterministic_signer("test-key");
    let mut registry = InMemoryKeyRegistry::new();
    registry.insert("test-key", signer.verifying_key());
    let verifier = Ed25519Verifier::new(registry);

    let policy = sample_policy();
    let bytes = canonicalize(&policy).unwrap();
    let (key_id, sig) = signer.sign(&bytes).unwrap();

    let mut tampered = bytes.clone();
    tampered[5] ^= 0x01;
    match verifier.verify(&key_id, &sig, &tampered) {
        Err(VerifyError::Invalid) => {}
        other => panic!("expected Invalid, got {other:?}"),
    }
}

#[test]
fn verify_rejects_wrong_signature() {
    let signer = deterministic_signer("test-key");
    let mut registry = InMemoryKeyRegistry::new();
    registry.insert("test-key", signer.verifying_key());
    let verifier = Ed25519Verifier::new(registry);

    let (_k, sig_for_a) = signer.sign(b"canonical bytes A").unwrap();
    // Verify against *different* bytes — signature is over A.
    match verifier.verify("test-key", &sig_for_a, b"canonical bytes B") {
        Err(VerifyError::Invalid) => {}
        other => panic!("expected Invalid, got {other:?}"),
    }
}

#[test]
fn verify_rejects_invalid_signature_length() {
    let signer = deterministic_signer("test-key");
    let mut registry = InMemoryKeyRegistry::new();
    registry.insert("test-key", signer.verifying_key());
    let verifier = Ed25519Verifier::new(registry);

    let bogus = vec![0u8; 63];
    match verifier.verify("test-key", &bogus, b"anything") {
        Err(VerifyError::InvalidSignatureLength) => {}
        other => panic!("expected InvalidSignatureLength, got {other:?}"),
    }
}

#[test]
fn from_bytes_round_trip() {
    let signer = Ed25519Signer::from_bytes("seed-key", &[7u8; 32]);
    let mut registry = InMemoryKeyRegistry::new();
    registry.insert("seed-key", signer.verifying_key());
    let verifier = Ed25519Verifier::new(registry);

    let msg = b"the bytes under test";
    let (key_id, sig) = signer.sign(msg).unwrap();
    assert_eq!(key_id, "seed-key");
    verifier.verify(&key_id, &sig, msg).unwrap();
}

#[test]
fn signer_new_with_explicit_signing_key() {
    // Exercise the non-from-bytes constructor path using rand's OsRng
    // so we cover realistic (non-deterministic) key material too.
    let mut csprng = rand::rngs::OsRng;
    let sk = SigningKey::generate(&mut csprng);
    let signer = Ed25519Signer::new("rand-key", sk);

    let mut registry = InMemoryKeyRegistry::new();
    registry.insert("rand-key", signer.verifying_key());
    let verifier = Ed25519Verifier::new(registry);

    let policy = sample_policy();
    let bytes = canonicalize(&policy).unwrap();
    let (key_id, sig) = signer.sign(&bytes).unwrap();
    verifier.verify(&key_id, &sig, &bytes).unwrap();
}

#[test]
fn canonicalize_policy_is_stable_across_clones() {
    // Two clones of the same policy must canonicalize identically, and
    // signing/verification over each must succeed interchangeably.
    let signer = deterministic_signer("test-key");
    let mut registry = InMemoryKeyRegistry::new();
    registry.insert("test-key", signer.verifying_key());
    let verifier = Ed25519Verifier::new(registry);

    let p1 = sample_policy();
    let p2 = p1.clone();
    let b1 = canonicalize(&p1).unwrap();
    let b2 = canonicalize(&p2).unwrap();
    assert_eq!(b1, b2);

    let (key_id, sig) = signer.sign(&b1).unwrap();
    verifier.verify(&key_id, &sig, &b2).unwrap();
}
