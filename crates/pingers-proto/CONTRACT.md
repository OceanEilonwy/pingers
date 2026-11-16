# pingers-proto contract

Wire types and encodings. No crypto; no logic beyond encode, decode and length checks.

## Fixed (other crates build against these)

- **Identifiers** (`ids.rs`): `PartyId` (16 B), `SpotId` (16), `GroupId` (32, ordered),
  `MailboxId` (16), `AccountId` (16), `SubscriptionId` (16), `LeaseId` (16), `PushHandle` (8),
  `ChatMessageId` (16), `LocationRequestId` (16), `RecordRandomness` (16). Each has
  `from_bytes`, `as_bytes`, `LENGTH`, and a `Debug` that prints `Name(redacted)`.
- **Counters**: `Epoch` (u64), `LeafIndex` (u32), `Lamport` (u64), `LocationGeneration` (u32),
  `SequenceNumber` (u64), `RelayId` (u16), each with `new`, `get`, `checked_next`.
- **Board**: `Record` (96 B, `to_bytes`/`from_bytes`/`parse_concatenated`), `RecordTag`,
  `SealedBeacon`, `BoardBeacon` (48 B), `MeetupHint`, `Lease` (424 B, `SLOTS = 4`),
  `RevokeCommitment`, `SegmentHash`, `SegmentInfo`, `BoardCursor`, `BoardPage`.
  Fixed layouts are implemented and must not change.
- **Knock**: `Knock`, `KnockSignature`.
- **Push and mailboxes**: `MailboxSpec`, `SealedPush`, `SealedPushBlob` (320 B), `RelayRef`,
  `RelayUrl`, `KeyConfigHash`, `Urgency`, `PushPlatform` (FCM, UnifiedPush), `PushClass`,
  `PushToken`, `PushTokenPlaintext` (`PADDED_SIZE = 272`), `Subscription`, `TimeToLive`.
- **Groups**: `CreateGroup`, `CommitRequest`, `CommitResponse`, `AppMessage` (the group
  message envelope), `GroupLogEntry`, `LiveFrame`, `ContentType`, `Delivery`, `GroupAuthKey`,
  and opaque `KeyPackageBytes`, `MlsMessageBytes`, `WelcomeBytes`, `SealedSnapshot`.
- **Meetup frames**: `MeetupMessage` (`SealedLocation`, `LocationGrant`, `LocationAsk`, `Chat`,
  `MeetPoint`, `Snapshot`, `BoardSeen`, `KeepAlive`), `MeetPoint`, `SealedLocationFrame`,
  `SealedGrant`, `SealedAsk`, `LocationPlaintext`, `E7Coordinate`, `AccuracyMeters`,
  `HeadingDegrees`, `SpeedCentimetresPerSecond`, `ChatBody`, `MeetPointLabel`, `SeenSegment`.
- **Auth**: `RlnAuth` (265 B), `RlnScope`, `ServerId`.
- **API**: `Params`, `PushRelayInfo`, `AnonymousCredential`, `Slashing`, `CustomPushRelays`,
  `CoverReadRatio`, `RegisterRequest`, `AuthRequest`, `BoardSyncRequest`, `LeaseReplacement`,
  `ReadBoard`, `BoardSyncResponse`, and the response bodies listed under "Encoding" below.
- **Padding** (implemented): `SizeClass` (`Bytes256` .. `Bytes32768`, `MEETUP_FRAME`,
  `MAILBOX_POST`), `Padded` (`pad`, `pad_smallest`, `from_wire`, `payload`).
- **Codec traits**: `WireEncode::encode(&self) -> Result<Vec<u8>, ProtoError>` and
  `WireDecode::decode(&[u8]) -> Result<Self, ProtoError>`; `decode(encode(x)) == x`, and
  decoders reject non-canonical input.
- `ProtoError` variants listed in `error.rs`; `Redacted<T>`.

## Implementer decides

- The CBOR layout of every `WireEncode`/`WireDecode` type (map keys or arrays, field numbers),
  as long as it is canonical (deterministic, shortest-form integers) and versioned: unknown
  versions are rejected with `ProtoError::UnsupportedVersion`.
- The `PushTokenPlaintext` padded layout and the fixed layout of `RlnAuth` (field order is
  fixed by design D: root, scope, bucket, x, y, nullifier, proof).
- The remaining API structs of design D (Privacy Pass issuance, membership leaves, mailbox
  reads, group state, invite payloads), added in `api.rs` in the same style.
- Whether field structs keep public fields or move to constructors, where an invariant appears.
- Fuzz targets for every decoder and `proptest` round trips (required by the style guide).

## Decisions

- **`PartyId` is 16 random bytes**, separate from the Party's 32-byte MLS `GroupId`
  (design C.2 lists both columns; neither size is given for `party_id`).
- **`LeaseId` is 16 bytes** and **`RelayId` a `u16`**; the design does not give sizes.
- **`ServerId` is the server's `https://` origin string** (design B.4.8 names `server_id`
  without defining it).
- **Padding format**: `be16(payload length) || payload || zero bytes` up to the class size.
  The design gives classes but no format.
- **Integers on the wire are big-endian**, matching `be64(b0)` in the derivations.
- **Timestamps** (`Chat.written_at`, `LocationPlaintext.taken_at`) are `UnixSeconds`; the
  design's `ts: u64` gives no unit.
- **`MeetupMessage::BoardSeen`** is added: design B.4.3 says members put the segment hashes
  they saw into the first Meetup message, but the B.6 enum has no variant for it.
- **`PushPlatform` has only FCM and UnifiedPush**, as the plan has no iOS app; the enum is
  `#[non_exhaustive]`.
- **`PushClass::Location`** keeps the design's name; on Android it means "high-priority FCM
  data message for a liveness check".
- **`ChatBody` is at most 3,500 bytes** and `MeetPointLabel` at most 120, so a frame fits the
  4 KiB class; the design gives no limits.

### Encoding (implemented 2026-11-13)

- **CBOR layout**: every top-level message is `[version, body]` with version `1`; bodies are
  definite-length arrays of the fields in declaration order (no maps). `None` is `null`;
  fixed-size byte arrays are byte strings of exactly that length; enums with data are
  `[variant, fields...]`; two-state enums are `0`/`1`. Each impl's doc comment in the source
  gives its layout, and `vectors/proto/` pins them. Nested structures carry no version.
- **Canonical check**: decoders parse with `minicbor`, then re-encode and compare. Anything
  that is not byte-for-byte the canonical encoding (long-form integers, indefinite lengths,
  `f16` floats, `-0.0`) is rejected. `decode` accepts exactly one encoding per value.
- **New `ProtoError` variants** (additive; the enum is `#[non_exhaustive]`): `NonCanonical`
  and `TrailingBytes`. `NotImplemented` is removed. A CBOR array with the wrong number of
  elements, a wrong CBOR type or invalid UTF-8 is `Cbor(..)`; an unknown enum value is
  `OutOfRange { field }`; a fixed-size field of the wrong length is `WrongLength`.
- **`PushTokenPlaintext`** padded layout: `version (1) || platform (1) || class (1) ||
  be64(not_after) || be16(token length) || token || zero bytes` to 272 bytes. Version 1;
  platform 1 FCM, 2 `UnifiedPush`; class 0 Standard, 1 Location. Tokens up to
  `MAX_TOKEN_BYTES` (259). `encode_padded` reports an over-long token as
  `OutOfRange { field: "push token" }`, not `PayloadTooLarge` as the stub's doc said:
  `PayloadTooLarge` names a `SizeClass`, and 272 bytes is not one. The signature is unchanged.
- **`RlnAuth`**: the 265-byte layout in design D's field order, scope as one byte, bucket as
  `be64`; no version byte (the length is the version).
- **`BoardPage`**: `[segments, records, next]`, records as one byte string of concatenated
  96-byte records. Both `encode` and `decode` reject a page with more than `MAX_RECORDS`
  records, or whose segments' `record_count`s do not add up to the number of records
  (`OutOfRange`). Builders of pages (pingers-board-store) must keep the counts consistent.
- **`CoverReadRatio`** is a CBOR `f32` (`0xfa`). `-0.0` is written as `0.0`.
- **Wire values**: `Urgency` Alert 0, Quiet 1; `Delivery` Stored 0, Ephemeral 1;
  `ContentType` uses the MLS values (1 application, 2 proposal, 3 commit);
  `AnonymousCredential` MembershipProof 0, PrivacyPass 1; `Slashing`, `CustomPushRelays` and
  `ReadBoard` are 0 for off, not allowed and skip.
- **Added API bodies** (design D): `RegisterResponse`, `AuthResponse`, `MembershipLeafRequest`,
  `MembershipLeafResponse`, `MembershipLeaves` (with `HourlyRoot`, `MembershipIndex`,
  `RootEpoch`), `MailboxMessages` (`MailboxMessage` carries a `Padded` body, so its length
  must be a size class), `MessageAccepted`, `GroupMessages`, `GroupState`. Privacy Pass
  issuance and invite payloads were added later (below).
- The unused `serde` dependency is removed.

### Push relay API (added 2026-11-13, wave 2)

- **`PushRequest`** (`POST /v1/push`) and **`PushRelayConfig`** (`GET /v1/config`) moved here
  from pingers-push-relay, which had them as JSON. They are CBOR now like every other body
  (design D: "bodies in CBOR unless noted"): `PushRequest` is `[key id, blob (320), bind (32),
  urgency, handle (8)]`; `PushRelayConfig` is `[key config (41 bytes), [platform...], max ttl
  seconds, operator]`. The design's `kem`/`kdf`/`aead`/`public_key` fields are carried as the
  RFC 9458 key config bytes (`PushRelayKeyConfig`, fixed suite X25519 / HKDF-SHA256 /
  AES-128-GCM, other suites `OutOfRange`), because SHA-256 of exactly those bytes is what
  members pin. `max_ttl_s` is a `TimeToLive`.
- **`PushRelayKeyId`** (`u8`, `new`/`get`) replaces the relay's own `KeyId`.
- **`SealedPush` carries `key_id: PushRelayKeyId`** (a new public field, so struct literals
  must add it): layout `[relay, relay key id, blob, urgency, handle]`. pingers-crypto binds the
  key id into the seal, so the device states which relay key it sealed to and the server
  forwards that id instead of substituting its configured current key.
- **`PushBind`** moved here from pingers-crypto so `PushRequest` can carry it; pingers-crypto
  re-exports it, so `pingers_crypto::PushBind` still works and the derivations stay there.
- `PushPlatform`'s wire values and `TimeToLive`'s CBOR helpers became `pub(crate)` for reuse.
- Vectors `push-request-encoding` and `push-relay-config-encoding`; fuzz targets
  `push_request` and `push_relay_config`.

### Privacy Pass issuance (added 2026-11-13, wave 2)

- **`PrivacyPassIssueRequest`** and **`PrivacyPassIssueResponse`** (`POST /v1/pp/issue`): each
  `[bytes]`, the bytes being pingers-anon's RFC 9578 batched issuance message (pingers-anon
  builds on `voprf` directly: request `token_type || truncated_key_id ||
  blinded_elements<2-byte length>`, response `evaluated_elements<2-byte length> ||
  proof(96)`). Opaque here, as pingers-anon's contract says ("TLS presentation inside the CBOR
  body"); constructors refuse empty messages and anything longer than a batch of
  `MAX_TOKENS_PER_ISSUANCE` (300) tokens (`OutOfRange`). The design's response also lists
  `key_id`; it is left out because the client already chose the key (the request carries its
  truncated id) and pingers-anon's `finish` takes only the response bytes.
- Vectors `privacy-pass-issue-request-encoding` and `privacy-pass-issue-response-encoding`;
  fuzz targets `privacy_pass_issue_request` and `privacy_pass_issue_response`.

### Wire secrets (wave 1 review, 2026-11-13)

- **`SecretBytes<N>`** and **`SecretText`** carry secrets on the wire: zeroed on drop
  (`zeroize`), no `PartialEq`, `Default` or `Copy`, compared only through
  `subtle::ConstantTimeEq`, `Debug` printing only the type, read with `expose()`. Two new
  dependencies, `subtle` and `zeroize` (already in the workspace; no primitives).
- **Signature changes** (the review asked for them): `LeaseReplacement::revoke_key` is
  `SecretBytes<32>` (was `Redacted<[u8; 32]>`), `AuthResponse::bearer` is `SecretText` (was
  `Redacted<String>`), `RegisterRequest::registration_code` is `Option<SecretText>` (was
  `Option<Redacted<String>>`), and `InvitePayload::invite_secret` is `SecretBytes<32>`. These
  structs keep `PartialEq`/`Eq`, implemented by hand so the secret field compares in constant
  time. The server checks a revealed revoke key with pingers-crypto's `revoke_key_matches`.
  `Redacted` stays for identifiers; its doc now says it is not for secrets.

### Invites (added 2026-11-13, wave 2)

- **`InvitePayload`**, the link fragment (`https://<host>/j#v1.<base64url(encoding)>`):
  `[server (ServerId text), invite mailbox (16), invite secret (32, SecretBytes), inviter
  fingerprint (8), expires (UnixSeconds)]`, plus a sixth element, the operator's registration
  code (`Option<SecretText>`, never empty), only when present (r-accounts). Join flow A
  (2026-11-14) removed the design's `single_use` (and `InviteUses`): every invite waits for
  the inviter and stops working once one joiner is let in, and nothing about the Party is in
  the link. `InviterFingerprint` is an 8-byte newtype with
  a redacted `Debug`. The `v1.` prefix and base64url are left to pingers-parties, so this crate
  needs no base64 dependency; the CBOR carries its own version.
- **`JoinRequest`**, the plaintext pingers-crypto's `seal_join_request` seals:
  `[key package, X-Wing key package or null, display name, reply mailbox (16), party
  subscription or null]`. The X-Wing slot (owner decision 2026-11-14) lets a joiner serve a
  quantum-resistant Party without the link naming the suite. The
  subscription is optional since join flow A: a joiner with wake-up pushes off still joins. The AEAD under
  `HKDF(invite_secret, "join")` is what authenticates the joiner's `KeyPackage` under the
  secret; this crate only encodes it. `DisplayName` is 1 to 200 bytes of UTF-8 (the design
  gives no limit; 200 matches pingers-store's `MemberName`) with a redacted `Debug`.
- **`InvitePost`** (join flow A), what is sealed and posted to the invite mailbox: `[0, join
  request]` or `[1, reply mailbox]` (`Withdraw`, the joiner's "Withdraw request").
- Vectors `invite-payload-encoding`, `join-request-encoding` and `invite-post-encoding`; fuzz
  targets `invite_payload`, `join_request` and `invite_post`. The files come from `cargo xtask vectors`
  (`xtask/src/vectors/proto/invite.rs`).

### Privacy Pass keys (added 2026-11-13, wave 3 server)

- **`PrivacyPassKeys`** (`GET /v1/pp/keys`) and **`PrivacyPassKey`** (additive): `[[key...]]`,
  each key `[key id (32), public key (bytes), issue until, redeem until]`, oldest first, the
  last one issuing. The design lists `keys: Vec<PublicKeyInfo>` without fields; these are the
  fields pingers-quota's `IssuerPublicKey` has and the client needs (the public key for
  `begin_token_issuance`, when to fetch the next). A unit round trip pins the layout; the
  vector and fuzz target are still to add.

### Wave 3 cleanup (2026-11-13)

- **One owner per concept.** `SingleSuiteKeyConfig` is the only RFC 9458 key config encoder;
  `PushRelayKeyConfig` and pingers-ohttp both use it, so the pinned hashes cannot drift. The
  hourly root record is now `HourlyRoot` (`MembershipRoot` stays as an alias), leaving
  "membership root" to pingers-anon's root value. `MembershipIndex` and `RootEpoch` (u32) are
  the only leaf position and root epoch types; pingers-anon and pingers-store alias or
  re-export them. Additive, except that pingers-store's `RootEpoch` is now this `u32` type.

### Party state (added 2026-11-14, v1-parties; pingers-parties request 1)

- **`PartyOperation`**, the plaintext of a Party group application message before padding
  (design B.2): `[0, ProfileUpdate]` or `[1, SpotState]` (`#[non_exhaustive]`).
  `ProfileUpdate` is `[name (DisplayName), subscription id (16) or null]`: the sender's roster
  profile and its Party group subscription, so whoever removes the sender can list it in
  `subscriptions_removed`. `SpotState` is `[spot id (16), name (SpotTitle), fence, created by
  (MemberKey, 32), lamport, life (0 Active, 1 Deleted)]`.
- **`SpotFence`**: `[0, latitude, longitude, radius metres]` or `[1, [[latitude, longitude],
  ...]]`. Numbers are always eight-byte floats (`0xfb`, never shortened: the decoder rejects
  other widths), so every member stores the sender's pingers-geo values bit for bit and
  validates the same geometry; `FencePoint` and the radius refuse NaN and infinities
  (`OutOfRange`). Ranges, polygon validity and the minimum radius are pingers-geo's and
  pingers-parties' checks.
- **`PartySnapshot`**: `[party id (16), name (PartyTitle), [[member key (32), ProfileUpdate],
  ...], [SpotState, ...]]`, the state the committer seals under the next epoch's state key.
  Members' subscriptions travel in it, so a member who joined later can still drop a removed
  member's subscription.
- `PartyTitle` and `SpotTitle` are 1 to 200 bytes of UTF-8 (as pingers-store's names);
  `MemberKey` wraps the 32-byte account key. All four print redacted `Debug`.
- Vectors `party-operation-encoding` and `party-snapshot-encoding` (generated by `cargo xtask
  vectors`, xtask/src/vectors/proto/party.rs, independently of this crate; first written by hand from the
  layout); fuzz targets `party_operation` and `party_snapshot`.
