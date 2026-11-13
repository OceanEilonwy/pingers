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
- **The account key is the leaf signature key** in every group (design B.1).
