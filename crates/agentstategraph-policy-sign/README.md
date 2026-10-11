# agentstategraph-policy-sign

Ed25519 signing and verification for
[`agentstategraph-policy`](https://crates.io/crates/agentstategraph-policy)
policies. An opt-in sibling crate, so consumers that never sign policies don't
compile `ed25519-dalek` and `sha2`.

- `canonicalize` produces the deterministic bytes that are signed and verified
  (sorted keys, no whitespace, UTF-8).
- `PolicySigner` / `Ed25519Signer` produce a `(signer_key_id, signature)` pair.
- `PolicyVerifier` / `Ed25519Verifier` check that pair against a `KeyRegistry`.

Part of [AgentStateGraph](https://github.com/agentstatelabs/agentstategraph). Most applications depend on the
[`agentstategraph`](https://crates.io/crates/agentstategraph) crate, which
re-exports the core types and storage backends.

## License

Licensed under either of
[Apache License, Version 2.0](https://github.com/agentstatelabs/agentstategraph/blob/main/LICENSE-APACHE)
or [MIT license](https://github.com/agentstatelabs/agentstategraph/blob/main/LICENSE-MIT),
at your option.
