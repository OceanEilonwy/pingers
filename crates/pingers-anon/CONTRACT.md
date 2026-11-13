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

### Implementation (2026-11-13)

- **Additions, no fixed signature changed.** `IdCommitment::leaf()` (the server needs
  `rate_commitment = Poseidon(id_commitment, 8)` to append on `POST /v1/rln/leaf`, and nothing
  else exposed it); `AcceptedRoots::{first, advance, accepts}` (the hourly root history);
  `MerklePath::position()`; `IssuerKey::expose_bytes()` (a generated key had no way to reach the
  key store). Each is additive; flagged for pingers-quota and pingers-store.
- **`rln` is no longer optional.** `RlnSecret::id_commitment` and `external_nullifier` are
  always available and are Poseidon hashes, so the default build needs `rln` too. `ark-ff`
  (scalar arithmetic) and `sha2` are plain dependencies; `p384` (the VOPRF group) and `subtle`
  come with `prover`/`issuer`. `ark-ff` and `p384` were added to the workspace dependencies.
- **Parameters are compiled in.** The proving key and witness graph are `rln` 3.1.0's embedded
  `tree_depth_20/rln_single` (`RLN(20, 16)`, 5,820 constraints); no constructor takes bytes
  because nothing is read at run time. Its verifying key equals, point for point, the output
  of PSE's RLN Trusted Setup Ceremony (p0tion ceremony `rln-trusted-setup-ceremony`, circuit
  rln-20, circom-rln `17f0fed`, 60 contributions plus a beacon, finalised 2023-09-04; final zkey
  sha256 `ae30d3d4…7946a`). `circuit::tests` pins `vk_delta_2`. The message limit 8 is in the
  leaf, not the circuit (the circuit range-checks `message_id < limit < 2^16`), so the same key
  serves k = 8.
- **The tree is our own** in-memory Poseidon tree (zero leaves, `Poseidon(left, right)`),
  updated level by level per batch; `apply` is all-or-nothing. `rln`'s `PmTree` (sled) is never
  used.
- **Proof randomness.** `rln` samples the Groth16 blinding scalars from its own thread-local
  CSPRNG and offers no way to pass them in. `prove` then re-randomises the proof with scalars
  from the injected `rng` (`A/r1, r1·B + r1·r2·δ, C + r2·A`), so zero-knowledge never rests on
  that generator alone. Proof bytes are therefore not reproducible; `y` and `nullifier` are, and
  are in `vectors/rln.json`.
- **`verify` order:** root window (`StaleRoot`), then `x` (`WrongRequest`), then canonical
  public inputs, point decoding and the pairing check (all `BadProof`).
- **Encodings** (`vectors/rln.json`): field elements 32-byte little-endian canonical;
  `external_nullifier = Poseidon(SHA-256("pingers/v1/rln" ‖ u64be(len) ‖ server_id ‖ scope ‖
  u64be(bucket)) mod r)`; `request_digest = SHA-256("pingers/v1/rln/x" ‖ u64be(len) ‖ method ‖
  u64be(len) ‖ path ‖ body) mod r`, hashes read little-endian. The labels and the SHA-256
  framing are pingers-crypto's (`rln_external_nullifier_hash`, `rln_request_hash`, wave 3); the
  field reduction and Poseidon stay here.
- **Privacy Pass encodings** (`vectors/privacy-pass.json`): P384-SHA384 VOPRF; public key 49-byte
  compressed point, `token_key_id = SHA-256(pkI)`; request `token_type ‖ truncated_key_id ‖
  blinded<2-byte length>`, response `evaluated<2-byte length> ‖ proof(96)`; token
  `0x0001 ‖ nonce ‖ challenge_digest ‖ key_id ‖ authenticator(48)`. Tokens are fetched before any
  challenge exists, so all answer one fixed `TokenChallenge` (issuer name `pingers`, empty
  redemption context and origin info); the spent set keyed by nonce is what makes them
  single-use. The `Authorization` value is `PrivateToken token="<base64url>"`.
- **sled cannot be removed.** `rln` 3.x depends on `sled` (feature `compression`) for every
  non-wasm target, unconditionally; `rln` 2.x and earlier with `stateless` still get it through
  `zerokit_utils`, which lists it unconditionally in every release. Removing it needs a fork.
  No sled code is reachable: `nm` finds no `sled::` symbol in the release benchmark binary.
  The `deny.toml` ignores that come from rln are all still needed (reasons updated).
- **Performance**: see `BENCHMARK.md`.
- **Wave 3 (2026-11-13):** `LeafPosition` is now an alias of `pingers_proto::MembershipIndex`
  (same `new`/`get`), so wire positions need no conversion.
