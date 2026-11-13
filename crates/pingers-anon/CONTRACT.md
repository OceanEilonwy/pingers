# pingers-anon contract

RLN-v2 membership proofs and Privacy Pass type 0x0001. Features: `prover` (client), `verifier`
(server), `issuer` (server).

## Fixed (other crates build against these)

- Always available: `RlnSecret` (`generate`, `from_bytes`, `expose_bytes`, `id_commitment`),
  `MessageId` (below `MESSAGE_LIMIT = 8`, `next`), field elements `IdCommitment`, `LeafValue`,
  `MembershipRoot`, `Nullifier`, `RequestDigest`, `ExternalNullifier`;
  `external_nullifier(&ServerId, RlnScope, Bucket)`, `request_digest(method, path, body)`.
- `prover` or `verifier`: `MembershipTree` (`empty`, `apply(&[LeafUpdate])`, `root`,
  `leaf_count`, `path`), `LeafUpdate`, `LeafPosition`, `MerklePath`.
- `prover`: `prove(&ProofRequest, rng) -> RlnAuth`; Privacy Pass client
  `begin_token_issuance(issuer_public_key, TokenCount, rng) -> PendingIssuance`,
  `PendingIssuance::{request, finish}`, `PrivacyPassToken` (`from_bytes`, `as_bytes`,
  `authorization`).
- `verifier`: `verify(&RlnAuth, &VerificationContext) -> VerifiedProof`, `AcceptedRoots`,
  `NullifierRecord`, `classify_nullifier` (implemented) -> `NullifierOutcome::{Fresh, Replay,
  Conflict}`.
- `issuer`: `IssuerKey` (`generate`, `from_bytes`, `public_key`, `key_id`), `issue_tokens`,
  `check_redemption` (returns the nonce; the caller owns the spent set).
- `AnonError` variants. The wire header is `pingers_proto::RlnAuth`.

## Implementer decides

- Field element encoding (little-endian canonical, as zerokit uses, unless vectors say
  otherwise), the hash for `request_digest` (SHA-256 reduced into the field) and the exact
  `external_nullifier` preimage encoding, all recorded in `vectors/`.
- How the proving key and circuit parameters are loaded (bundled bytes passed in; no file
  system access below L2). If loading needs I/O, add a constructor that takes the bytes.
- Privacy Pass wire encodings (RFC 9578 TLS presentation inside the CBOR request body).

## Decisions

- **Privacy Pass is built on `voprf` directly.** `privacypass 0.2.0-pre.4` does not compile:
  its `blind-rsa-signatures` dependency breaks against the current `rsa`. The architecture
  already lists `voprf` as the dependency.
- **The tree is shared by prover and verifier** so the server and client compute identical
  roots from the same leaf list.
- **Nullifier classification is pure** and lives here; pingers-quota stores the set.
- **Slashing is not implemented**: the launch default is `rln_slashing = off` (design B.4.8);
  `NullifierOutcome::Conflict` is a plain rejection.
