# pingers-mls contract

A narrow wrapper over OpenMLS 0.9. No other crate touches OpenMLS types.

## Fixed (other crates build against these)

- `trait MlsStorage: Send + Sync { read, write, delete }` over opaque byte keys and values
  (`Zeroizing<Vec<u8>>` on read), with `StorageError::{Unavailable, Corrupt}`.
  pingers-store implements it.
- `MlsContext::new(&storage, &AccountSigningKey)` and its methods: `create_key_package`
  (`KeyPackagePurpose::Party` or `::Meetup { grant_key }`), `create_party_group`,
  `join_party_group`, `party_group`, `create_meetup_group(&MeetupCreation) -> CreatedMeetup`,
  `join_meetup_group(welcome, &MeetupPskBinding, &Roster)`, `meetup_group`, `delete_group`.
- `Group<'_, S, Party>` / `Group<'_, S, Meetup>` (typestate). Common: `group_id`, `epoch`,
  `own_leaf`, `roster`, `process`, `seal_application(&Padded)`, `remove_member`,
  `commit_pending_proposals`, `self_update`, `propose_leave`, `merge_pending_commit`,
  `discard_pending_commit`, `state_key`, `log_auth_key_pair`. Party only: `add_members`,
  `presence_root(&PartyId)`. Meetup only: `add_members(.., &Roster, ..)` with the roster check,
  `grant_keys`.
- Values: `Commit { commit, welcome }`, `CommitSummary`, `SelfRemoval`, `ProcessedMessage`,
  `MemberGrantKey`, `MeetupPskId { party, epoch, spot, bucket }`, `MeetupPskBinding`,
  `CipherSuitePolicy::{Classical, QuantumResistant}`, `Roster` (implemented), `RosterEntry`.
- `MlsError` variants.
- Exporter labels come from `pingers_crypto::ExporterLabel`; exporter outputs are wrapped in
  pingers-crypto's secret types (`PresenceRoot`, `StateKey`, `LogAuthKeyPair`).

## Implementer decides

- The adapter from `MlsStorage` to OpenMLS's `StorageProvider` and its key layout.
- How the `pingers_grant_key` extension is encoded (an unknown/custom leaf-node extension
  type; pick a private-use code point and document it here).
- The external PSK id encoding (the four fields in a fixed order).
- How X-Wing is wired (feature `x-wing`, libcrux provider); without the feature,
  `CipherSuitePolicy::QuantumResistant` returns `MlsError::UnsupportedSuite`.
- Group configuration details beyond design B.2 (out-of-order tolerance 50 for Meetup groups,
  per B.6).

## Decisions

- **Storage is a byte key-value trait**, not OpenMLS's `StorageProvider` itself, so
  pingers-store does not depend on OpenMLS. Errors are kinds without sources, because the
  trait must not leak the backend's error types or stored bytes.
- **Group handles borrow the context** (`Group<'context, S, Kind>`); a commit stays pending
  until the server accepts it (merge) or rejects it with 409 (discard).
- **The account key is the leaf signature key** in every group (design B.1). The leaf
  credential is a `BasicCredential` whose identity is the same 32-byte key.
- **Storage key layout** (adapter, `src/adapter.rs`): `"openmls/v1/" || label || "/" ||
  JSON(key)`, values JSON, as in OpenMLS's memory storage (labels such as `Tree`,
  `EpochSecrets`, `Psk`). The wrapper's own rows: `"pingers/v1/meetup-roster/" || group_id`
  holding the Party roster a Meetup checks adds against, as `be32(leaf) || key (32)` per entry.
  OpenMLS hides provider errors inside its own; the adapter records the first storage failure
  of an operation and the wrapper reports it as `MlsError::Storage` whatever OpenMLS made of it.
- **`pingers_grant_key` extension**: leaf-node extension type `0xF0A1` (RFC 9420 private-use
  range, not a GREASE value); `extension_data` is the raw grant public key. Meetup leaves list
  the type in their capabilities, and a Meetup's group context requires it
  (`RequiredCapabilities`), which is also how a stored group is recognised as a Meetup.
  Vectors: `vectors/mls/grant-key-extension.json`.
- **External PSK id**: `party_id (16) || be64(epoch) || spot_id (16) || be64(bucket)`, 48 bytes;
  the nonce is random per injection (RFC 9420). Vectors: `vectors/mls/meetup-psk-id.json`. The
  PSK value is stored only for the call that uses it and deleted right after.
- **Exporter contexts**: `state` uses `be64(epoch)`, `log-auth` the 32-byte group id,
  `presence` the 16-byte Party id.
- **Wire forms**: `KeyPackageBytes` is a bare TLS `KeyPackage`; `WelcomeBytes` and
  `MlsMessageBytes` are TLS `MLSMessage`s (the RFC 9420 framing with version and wire format).
- **Group configuration**: `PURE_CIPHERTEXT` wire format for sending and receiving (so external
  commits and external proposals, which are `PublicMessage`s, are refused, and there is no
  external-senders extension); the ratchet tree travels in the Welcome; no past-epoch secrets
  are kept (`max_past_epochs = 0`), so a message for another epoch is `WrongEpoch`; out-of-order
  tolerance 50 in Meetups and OpenMLS's default 5 in Parties, maximum forward distance 1000.
- **Standalone proposals**: the only one accepted is a member removing itself (a leaver);
  anything else is `InvalidMessage`. While such a proposal waits, `seal_application` returns
  `CommitPending` (OpenMLS refuses to send then); commit it first.
- **Randomness**: OpenMLS's `OpenMlsRand` draws from the caller's RNG. Calls that the contract
  gives no RNG (`process`, joins, merges) get a provider that refuses randomness. OpenMLS's
  crypto backend still draws HPKE encapsulation randomness from its own OS-seeded generator,
  so commits and Welcomes are not reproducible from a seed; only the wrapper's own encodings
  have vectors.
- **Roster check placement** (flag): `Group::process` has no roster parameter, so each Meetup
  stores the Party roster it was created or joined with, and checks every added, updated or
  committed leaf against it. Added (not changed) `Group<Meetup>::update_party_roster(&Roster)`
  to refresh it when the Party roster changes; `Meetup::add_members` also stores the roster it
  is given. Without the refresh, a member would refuse to see a newer Party member added.
- **Pending-epoch exports** (flag, added): `Group::pending_state_key` and
  `pending_log_auth_key_pair` give the keys of the epoch a pending commit moves to. Design B.2
  has the committer send the next epoch's log-auth public key and a snapshot sealed under the
  next state key with the commit, before it is merged.
- **Later Meetup joiners** (flag): design B.5 injects the PSK only at creation, and
  `Meetup::add_members` takes no PSK, so a later Add's Welcome carries none. `join_meetup_group`
  refuses a Welcome whose PSK differs from the binding, accepts one without a PSK on the roster
  check alone, and refuses more than one PSK.
- **X-Wing** (feature `x-wing`): `QuantumResistant` maps to
  `MLS_256_XWING_CHACHA20POLY1305_SHA256_Ed25519` (0x004D). OpenMLS 0.9.2 gates the suite
  behind the PQ-draft feature of `openmls_traits` (and of its providers), and only
  `openmls_libcrux_crypto` 0.4 implements it, so with the feature libcrux does all MLS crypto,
  for both suites. `openmls`'s own PQ feature is not enabled: it names its optional SQLite
  storage, whose rusqlite `links` clash with the workspace's; `openmls` has no PQ code outside
  its tests. Added `openmls_libcrux_crypto = "0.4.0"` to the workspace dependencies. Tests
  cover an X-Wing Party end to end and a Meetup with 1216-byte grant keys.
- **Storage failures**: the group handle may be ahead of the store after `MlsError::Storage`;
  callers drop it, roll back, and reload.
- **`MlsError::NotImplemented`** is no longer returned; kept so the variant list is unchanged.
