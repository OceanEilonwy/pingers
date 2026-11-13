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
  `ReadBoard`, `BoardSyncResponse`.
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
