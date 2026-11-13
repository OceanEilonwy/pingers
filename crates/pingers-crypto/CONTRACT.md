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
  (implemented), `seal_push_token`, `open_push_token`.
- **Groups**: `LogAuthKeyPair::public_key` (implemented), `sign_group_request`,
  `seal_state_snapshot`, `open_state_snapshot`.
- **Location**: `GrantKeyPair::generate`, `seal_location`, `open_location`, `dummy_location`
  (implemented), `seal_location_grant`, `open_location_grant`, `seal_location_ask`,
  `open_location_ask`.
- **Invites**: `seal_join_request`, `open_join_request`.
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
  proof). The design gives no label for these; domain separation needs one.
- **Grant public keys are variable-length bytes** so the X-Wing variant (feature `x-wing`)
  fits the same type.
- **Snapshots, join requests and knocks** carry a random 24-byte XChaCha20 nonce in front of
  the ciphertext.
