# pingers-crypto contract

The only crate that calls primitives. Labels are private constants in `labels.rs`, except the
three MLS exporter labels (`ExporterLabel`), which pingers-mls passes to OpenMLS.

## Fixed (other crates build against these)

- **Secret types** (Zeroizing, no `Debug`/`Clone`/`Serialize`): `PresenceRoot`, `SpotKey`,
  `TagKey`, `SealKey`, `KnockKey`, `MeetupPsk`, `AccountSigningKey`, `MailboxReadKey`,
  `PushRelayPrivateKey`, `StateKey`, `LogAuthKeyPair`, `LocationKey`, `GrantSecretKey`,
  `RevokeKey`, `InviteSecret`. Types another crate stores or hands to a library have public
  `from_bytes(Zeroizing<[u8; N]>)` and `expose_bytes`; the doc comment of each says who and why.
  Random ones have `generate(&mut impl CryptoRng)`.
- **Public types**: `AccountPublicKey`, `Signature`, `GrantPublicKey`, `GrantKeyPair`,
  `PushRelayPublicKey`, `PushBind`, `KnockSignedFields`, `GroupRequest`, `LocationAad`,
  `GrantContext`, `LocationGrant`, `OpenedGrant`, `LocationAsk`, `ExporterLabel`,
  `SEALED_LOCATION_LENGTH`.
- **Presence**: `derive_spot_key`, `derive_tag_key`, `record_tag`, `tag_matches`,
  `derive_seal_key`, `seal_board_beacon`, `open_board_beacon`, `derive_meetup_psk`,
  `derive_knock_key`, `seal_knock`, `open_knock`, `sign_knock`, `verify_knock`.
- **Board**: `revoke_commitment`, `segment_hash` (implemented).
- **Identity**: `account_id`, `AccountSigningKey::public_key` (implemented),
  `sign_registration`.
- **Mailboxes and push**: `mailbox_id`, `push_bind_for_mailbox`, `push_bind_for_subscription`
  (implemented), `seal_push_token`, `open_push_token` (with `PushTokenToOpen`).
- **Groups**: `LogAuthKeyPair::public_key` (implemented), `sign_group_request`,
  `seal_state_snapshot`, `open_state_snapshot`.
- **Location**: `GrantKeyPair::generate`, `seal_location`, `open_location`, `dummy_location`
  (implemented), `seal_location_grant`, `open_location_grant`, `seal_location_ask`,
  `open_location_ask`.
- **Invites**: `seal_join_request`, `open_join_request` (padded), `seal_join_request_in_class`,
  `join_code(&InviteSecret, key_package) -> JoinCode` (12 digits, `Display` in groups of four).
- Every fallible function returns `CryptoError`; unreachable primitive errors map to
  `CryptoError::Internal`, never a panic.

## Implementer decides

- Internal representation of `TagKey` (for example a precomputed HMAC state, so trial matching
  costs two compressions per record) and of the other secret types.
- Which RustCrypto/hpke-rs APIs to use, as long as the outputs match the formulas in the doc
  comments byte for byte.
- The test vectors in `vectors/` for every derived value and sealed format (required).
- Adding verify functions the server needs (for example group request signatures) with the
  same shape.

## Decisions

- **Same-layer dependencies on pingers-proto and pingers-time**, listed in the workspace
  manifest: keys are derived from proto's identifiers and `Bucket`, and seal proto's records.
- **Sealed knock size**: `seal_knock` pads inside the AEAD so the whole mailbox post (24-byte
  nonce, ciphertext, 16-byte tag) is exactly the size class (1 KiB, or 4 KiB for X-Wing).
  The design says "padded to 1 KiB" without saying whether before or after sealing; mailbox
  posts must be exactly 1, 4, 8 or 32 KiB.
- **Knock signature input**: fixed-width concatenation `label || party_id || be64(epoch) ||
  spot_id || be64(b0) || matched_r || SHA-256(key_package) || reply_mailbox ||
  SHA-256(subscription encoding)`, with 32 zero bytes when there is no subscription. The
  design lists the fields but not their encoding.
- **Sealed location length** is fixed at 80 bytes (64-byte padded plaintext plus tag), so
  optional heading and speed fields do not change the length and dummies match exactly.
- **AAD and info encodings** use big-endian integers: `be32` for leaf indexes and generations.
- **Labels not in the design**: `pingers/v1/lk-ask` (HPKE info for asks, with the group id),
  `pingers/v1/grp-request` (group request signatures), `pingers/v1/register` (registration
  proof), `pingers/v1/sub-bind` (subscription binds), `pingers/v1/relay-token` and
  `pingers/v1/relay-credential` (keyed hashes), `pingers/v1/board-cursor` (cursor AAD). The design gives no label for these;
  domain separation needs one.
- **Grant public keys are variable-length bytes** so the X-Wing variant (feature `x-wing`)
  fits the same type.
- **Snapshots, join requests and knocks** carry a random 24-byte XChaCha20 nonce in front of
  the ciphertext.
- **Grant and ask HPKE suite** (design B.6.1 names X25519 but no KDF or AEAD):
  DHKEM(X25519, HKDF-SHA256), HKDF-SHA256, ChaCha20-Poly1305, base mode, matching MLS suite
  0x0003. Under feature `x-wing` the KEM is X-Wing draft 06 (HPKE KEM id 0x647a) with the same
  KDF and AEAD. Sealed format is `enc || ciphertext`, AAD empty (bindings are in `info`). The
  suite is chosen by the recipient key's length when sealing and by the sealed length when
  opening; a grant secret key is 32 bytes for both (for X-Wing it is the seed).
- **Grant key pairs** are RFC 9180 `DeriveKeyPair` over 32 bytes from the caller's RNG.
  `GrantKeyPair::generate_x_wing` is added behind `x-wing`; `generate` stays X25519.
- **HPKE randomness is injected.** hpke-rs seeds its PRNG from the OS; pingers-crypto wraps
  the RustCrypto provider with a PRNG that replays bytes drawn from the caller's RNG (32 bytes
  of `ikmE` per X25519 seal, 64 per X-Wing seal), checked against RFC 9180 A.1.1. This needs
  two new workspace dependencies, `hpke-rs-crypto` (the provider trait) and `x-wing`
  (optional), and hpke-rs's `hazmat` feature (to read derived private-key bytes).
- **`PushBind` is defined in pingers-proto** (the push request carries it) and re-exported
  here unchanged; `push_bind_for_mailbox` and `push_bind_for_subscription` stay here.
- **Push relay `key_id` is bound in the HPKE `info`**: `info = "pingers/v1/push" || bind ||
  key_id` (one byte), which costs no blob space. The design puts only `bind` in `info`; without
  the key id, a blob sealed for one key would open when presented under another key id that
  maps to the same private key (an operator reusing a key under a new id). Opening therefore
  takes the key id: `open_push_token(&PushRelayPrivateKey, &PushTokenToOpen { key_id, bind,
  blob })` replaces `open_push_token(key, bind, blob)` (only pingers-push-relay calls it).
  `seal_push_token` is unchanged and takes the id from `PushRelayPublicKey::key_id`.
- **Subscription binds have their own label**, `pingers/v1/sub-bind` (design D says "as for a
  mailbox"). Mailbox binds keep the design's `pingers/v1/push-bind`. With one shared label a
  subscription and a mailbox whose ids had the same bytes would have the same bind, so a blob
  registered for one would open when presented for the other; the labels rule that out by
  construction instead of relying on random ids.
- **`revoke_key_matches(&SecretBytes<32>, &RevokeCommitment) -> bool`** (added, wave 1
  review): the server's constant-time check that `SHA-256(revealed) == commitment`.
  `KeyedHash` no longer derives `PartialEq`, `Eq` or `Hash`; compare with `ct_eq`, key maps
  on `as_bytes()`.
- **Board cursors** (added): `BoardCursorKey` (secret; `generate`, and `from_bytes` for tests), `seal_board_cursor
  (key, position, rng) -> BoardCursor` and `open_board_cursor(key, &BoardCursor)`: `nonce ||
  XChaCha20-Poly1305(key, aad = "pingers/v1/board-cursor", position)`. pingers-board-store had
  reused `seal_state_snapshot` under a cursor-only key; the label in the AAD keeps the two
  apart even under equal key bytes. Vectors in `vectors/crypto/board-cursor.json`.
- **Join requests are padded inside the AEAD** (wave 1 review, high): `nonce || XChaCha20(
  join_key, be16(len) || request || zeros)`, exactly one mailbox size class long, as knocks
  are (the two share `padded.rs`). `seal_join_request` keeps its signature and picks the
  smallest class that fits (a classical join request, about 650 bytes, takes 1 KiB; one with
  an X-Wing key package 4 KiB), so the post reveals only the class. Added
  `seal_join_request_in_class(secret, request, SizeClass, rng)` for callers that want a fixed
  class. `open_join_request` strips the padding and refuses non-zero padding.
- **Join code** (join flow A, 2026-11-14): `be64(HMAC-SHA256(HKDF-Expand(invite_secret,
  "pingers/v1/join-code", 32), be32(len) || key_package || be32(len) || x_wing_key_package)[..8])
  mod 10^12`, 12 digits (`JoinKeyPackages`; the X-Wing one empty when absent). Over both whole
  key packages (each carries and is signed by the joiner's signature key) so neither side needs MLS to
  compute it and the code names exactly the leaf that would be added. The modulo bias is below
  10^-7. Vectors in `vectors/crypto/join-code.json` (HKDF and HMAC steps checked with openssl).
- **Join key** is `HKDF-Expand(invite_secret, "join", 32)` without an Extract step: the invite
  secret is already 32 uniform bytes, as `PR_e` is for the Spot keys.
- **Group request signature input** uses `be32` length prefixes for method and path.
- **Signatures are verified with `verify_strict`** (no small-order keys, canonical encodings).
- **Location frames**: a ciphertext that is not 80 bytes gives `WrongLength`; the generation in
  the AAD is the caller's `aad.generation`, so a frame of another generation does not open.
- **Grants and asks are not padded here.** The 256-byte frame class is applied when the
  `MeetupMessage` is padded; an X-Wing grant (1,180 bytes) cannot fit 256 bytes and takes the
  4 KiB class.
- **Keyed hash for the push relay** (added): `KeyedHashKey` (secret, public `from_bytes` and
  `expose_bytes` because the relay reads it from its configuration), `KeyedHashPurpose`
  (`PushTokenRateLimit`, `ServerCredential`), `KeyedHash` (32 bytes, `ConstantTimeEq`, `Debug`
  redacted; no `==`) and `keyed_hash(key, purpose, input) =
  HMAC-SHA-256(key, label || input)`, labels `pingers/v1/relay-token` and
  `pingers/v1/relay-credential`. The relay keys its per-token rate limits with it in place of
  the design's `SHA-256(push_token)`, and stores server credentials as keyed hashes. Vectors in
  `vectors/crypto/keyed-hash.json`.
- **Additions** (no fixed signature changed): `verify_group_request` and
  `verify_registration` (server side), `PushRelayPrivateKey::generate` and
  `PushRelayPrivateKey::public_key(key_id)` (relay key config). `CryptoError::NotImplemented`
  is removed now that nothing is stubbed.
- **Vectors** live in `vectors/crypto/` and load as `load_vectors("crypto/<name>")`. Fields that
  need no AEAD or curve arithmetic (derivations, tags, hashes, signed messages, AAD, info) were
  recomputed independently with Python's standard library when the files were written.

- **Wave 3 (2026-11-13), additive:** `rln_external_nullifier_hash`, `rln_request_hash` and
  `RlnPreimageHash` take over the RLN preimage labels and framing from pingers-anon, which now
  only reduces the hash into the field and applies Poseidon. Outputs are unchanged
  (`vectors/rln.json` still passes).

### Account auth challenge (added 2026-11-13, wave 3 server)

- **`sign_auth_challenge` / `verify_auth_challenge`** (additive): the `nonce_sig` of
  `POST /v1/auth` is an Ed25519 signature by the account key over `"pingers/v1/auth" ||
  public key || be64(bucket index) || server origin`. Design D says "a signed challenge" but
  gives no challenge endpoint and `AuthRequest` carries no nonce, so the challenge is the
  current 15-minute bucket; pingers-server accepts the current bucket, or the previous one
  within 60 s of a rollover. A replay inside the window only gets the same account another
  bearer. New label `pingers/v1/auth` (decision).

### Safety numbers and inviter fingerprints (added 2026-11-14, v1-parties)

- **`safety_number(&pk_a, &pk_b) -> SafetyNumber`** (additive; pingers-parties request 3): the
  60-digit number of design B.1 over two leaf keys, the same in either order. Per key:
  `h = SHA-512("pingers/v1/safety-number" || pk)`, then `h = SHA-512(h || pk)` 5199 more
  times (Signal's work factor); `h[..30]` read as six big-endian 40-bit integers, each modulo
  10⁵, gives 30 digits; the bytewise smaller key's digits come first. `SafetyNumber::digits()`
  returns them; `Debug` is redacted. New label `pingers/v1/safety-number` (decision: the
  design gives no derivation). Vectors: `vectors/crypto/safety-number.json`, computed with
  openssl, not with this crate.
- **`inviter_fingerprint(&pk) -> InviterFingerprint`** (moved from pingers-parties):
  `SHA-256(pk)[..8]`, the first 8 bytes of the account id. Same vectors file.
