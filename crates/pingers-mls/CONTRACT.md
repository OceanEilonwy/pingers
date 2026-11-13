# pingers-mls contract

A narrow wrapper over OpenMLS 0.9. No other crate touches OpenMLS types.

## Fixed (other crates build against these)

- `trait MlsStorage: Send + Sync { read, write, delete, delete_prefix }` over opaque byte keys
  and values (`Zeroizing<Vec<u8>>` on read), with `StorageError::{Unavailable, Failed,
  Corrupt}`, and `group_storage_prefix(&GroupId)`. pingers-store implements it. (Changed
  2026-11-13 on pingers-store's feedback; see Decisions.)
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
- **Storage key layout** (adapter, `src/adapter.rs`): every row of a group starts with
  `group_storage_prefix(group_id)` = `"openmls/v1/group/" || JSON(OpenMLS GroupId) || "/"`,
  then `label` (one row per group: `Tree`, `EpochSecrets`, ...) or `label || "/" || JSON(item)`
  (per proposal, per epoch and leaf). Rows of no group (KeyPackages, PSKs, standalone key
  pairs) are `"openmls/v1/" || label || "/" || JSON(key)`. Values are JSON, as in OpenMLS's
  memory storage. The wrapper's own row `prefix || "pingers-meetup-roster"` holds the Party
  roster a Meetup checks adds against, as `be32(leaf) || key (32)` per entry. OpenMLS hides
  provider errors inside its own; the adapter records the first storage failure of an
  operation and the wrapper reports it as `MlsError::Storage` whatever OpenMLS made of it.
- **Trait change: `MlsStorage::delete_prefix(prefix)`** (pingers-store feedback 1). Keys are
  still opaque to the store, but all of a group's rows share `group_storage_prefix`, so
  `MlsContext::delete_group` is one `delete_prefix` (it also catches key pairs of epochs the
  group moved past), and the store can wipe an ended Meetup itself with
  `delete_prefix(&group_storage_prefix(&group_id)?)` in the same transaction (design B.5). It
  is a required method: a default that silently kept rows would defeat B.5. The store
  implements it as a range delete (`key >= prefix AND key < successor(prefix)`) or a
  byte-wise `starts_with`.
- **Trait change: `StorageError::Failed`** (pingers-store feedback 2): `Unavailable` (cannot
  reach or locked, retry later), `Failed` (reached, but the operation failed: I/O, full disk,
  constraint, aborted transaction), `Corrupt` (bytes do not decode). "Not found" is not an
  error: `read` returns `Ok(None)`, and a missing group is `MlsError::NotAMember`. No source
  error is attached on purpose: it would carry backend messages (possibly stored bytes or
  paths) into errors that reach the UI, against style guide 9; the store keeps details at its
  own API.
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
- **X-Wing is not wired on this branch** (open, owner's decision): `QuantumResistant` returns
  `UnsupportedSuite` even with the `x-wing` feature. OpenMLS 0.9.2 gates 0x004D behind the
  PQ-draft feature of `openmls_traits` and its providers, and only `openmls_libcrux_crypto`
  0.4.0 implements it. That wiring works (branch `w1-mls-x-wing`: libcrux does all MLS crypto
  under the feature; an X-Wing Party and a Meetup with 1216-byte grant keys pass the tests;
  `openmls`'s own PQ feature must be avoided because it names its optional SQLite storage,
  whose rusqlite `links` clash with the workspace's). But 0.4.0 pins libcrux-hmac-drbg 0.0.1
  and libcrux-kem 0.0.9, with open advisories RUSTSEC-2026-0329, -0330 and -0331 (panics; -0331
  is a short X-Wing public key from a malicious KeyPackage or path crashing the decoder), so
  `cargo deny check` fails with it in the graph. Merge that branch once upstream moves, or if
  the owner accepts the advisories for an off-by-default feature.
- **Storage failures**: the group handle may be ahead of the store after `MlsError::Storage`;
  callers drop it, roll back, and reload.
- **`MlsError::NotImplemented`** is no longer returned; kept so the variant list is unchanged.
- **Mutation testing** (`cargo mutants -p pingers-mls`): 14 survivors, all justified. 13 are
  adapter methods OpenMLS 0.9.2 calls only from APIs the wrapper never uses: the signature key
  store (`write/read/delete_signature_key_pair`, application-managed), update-proposal leaves
  (`append_own_leaf_node`, `own_leaf_nodes`, `delete_own_leaf_nodes`,
  `write/read/delete_encryption_key_pair`, used only by `propose_self_update`), single
  proposal removal (`remove_proposal` and its `remove_from_list` helper, two mutants, used only
  by `remove_pending_proposal`), and `queued_proposal_refs` (loading reads
  `queued_proposals`). They are implemented so the adapter stays a complete
  `StorageProvider`. The 14th replaces `Provider::crypto` with a fresh backend, which is
  equivalent: the crypto backend holds no state the wrapper relies on.
