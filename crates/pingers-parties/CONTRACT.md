# pingers-parties contract

Parties as long-lived things (client, L3). Sans network: every request for the server is an
`Outbound` value for pingers-sync, and everything from the server comes back in as a value.

## Fixed (other crates build against these)

- `PartyLifecycle::new(&Session, &dyn Clock)`; every method is one store transaction (MLS
  state and rows commit together).
- Parties: `create_party(NewParty, rng) -> CreatedParty { party, outbound: CreateGroup }`,
  `settings`, `set_relays`, `set_location_visibility`, `effective_location_visibility`,
  `leave_party -> Vec<Outbound>` (a `Leave` plus `DeleteMailbox` per invite).
- Roster: `roster -> Vec<RosterMember>`, `roster_view -> RosterView` (implements
  `pingers_presence::RosterLookup`; `current_epoch` is the newest merged epoch),
  `safety_code`, `verify_member`, `log_auth_key`, `remove_member`, `commit_pending_proposals`,
  `self_update` (each `-> OutgoingCommit`), `commit_accepted -> CommitApplied`,
  `commit_rejected`, `process_group_message(&GroupId, &MlsMessageBytes) -> Incoming`.
- Invites (inviter, join flow A): `create_invite(&PartyId, NewInvite { registration_code }, rng) -> CreatedInvite { invite,
  link, expires }`, `invite_status`, `invites` (with `shared_at`), `revoke_invite ->
  DeleteMailbox` (drops its waiting requests), `invite_read_position(InviteId) ->
  Option<SequenceNumber>`, `receive_join_request(InviteId, SequenceNumber, &Padded) ->
  JoinRequestOutcome::{AwaitingApproval(PendingJoin), Withdrawn(JoinId), Refused(Refusal),
  Ignored(Ignored)}`, `pending_joins -> Vec<PendingJoin { join, invite, name, code,
  asked_at }>`, `approve_join -> Admission { commit, close_invite: DeleteMailbox }`,
  `decline_join`.
- Joins (joiner): `prepare_join(&InviteLink, JoinDetails { display_name, subscription:
  Option }, rng) -> PreparedJoin { join, code, outbound: MailboxPost }`, `asked_join ->
  AskedJoin { join, code, asked_at }`, `withdraw_join -> [MailboxPost, DeleteMailbox]`,
  `welcome_access`, `accept_welcome(&ArrivedWelcome, rng) -> Joined { party, via, outbound }`,
  `joined_via(&PartyId) -> Option<JoinedVia { inviter, code }>`.
- Spots: `create_spot(&PartyId, NewSpot, rng)`, `edit_spot(&PartyId, SpotEdit, rng)`,
  `delete_spot` (each `-> SpotChange { spot, outbound: GroupMessage }`), `spots`;
  `SpotShape::{Circle, Polygon}::into_fence`.
- Profiles: `update_profile(&PartyId, ProfileChange, rng) -> ProfilePublication`,
  `publish_waiting_profile -> Option<Outbound>`, `profile_waiting`; `upgrade_records`;
  `PostQuantum::suite() -> CipherSuitePolicy` (public).
- `Outbound::{CreateGroup, Commit(OutgoingCommit), GroupMessage, MailboxPost, DeleteMailbox,
  Leave(LeaveRequest)}`; `InviteLink` (`new`, `from_text`, `as_str`, `payload`);
  `SafetyCode`; `inviter_fingerprint`; `INVITE_LIFETIME` (24 h); `PartiesError`.

## What pingers-sync carries

| Value | Request | Then |
|---|---|---|
| `Outbound::CreateGroup` | `POST /v1/groups` (Anon, scope Group) | — |
| `Outbound::Commit` | `POST /v1/groups/{id}/commit`, signed with `log_auth_key` | 200: `commit_accepted`, then post `welcome` to its mailbox. 409: `commit_rejected`, catch up, retry |
| `Outbound::GroupMessage` | `POST /v1/groups/{id}/msgs` (signed) | — |
| `Outbound::MailboxPost` | `POST /v1/mailboxes/{id}/msgs` | — |
| `Outbound::DeleteMailbox` | `DELETE /v1/mailboxes/{id}` with the row's read key | delete the store row |
| `Outbound::Leave` | the proposal to `/msgs`, then `DELETE /subs/{id}`, both signed with `auth` | — |

Inputs: posts to an invite mailbox, read after `invite_read_position`, go to
`receive_join_request` in order; the Welcome in a reply
mailbox plus `GET /v1/groups/{id}/state` go to `accept_welcome`; every group log entry goes
to `process_group_message` in log order, except entries this device posted itself (their
`seq` came back from the post): an own commit is already merged by `commit_accepted` and
an own application message cannot be decrypted by its sender (both are errors). pingers-sync keeps the mailbox pool: invites take a
pooled `MailboxPurpose::Invite` mailbox (registered with an Alert sealed push) and joins a
pooled `Reply` one; with none, `NoPooledMailbox`.

## Decisions

- **One commit in flight per Party.** A second is `CommitInFlight`. A rejected Add puts the
  join request back in `pending_joins` (the invite stays used by that joiner) so it can be
  committed again after catching up.
- **Join flow A** (2026-11-14): the link carries nothing about the Party. Every request waits
  for the inviter with its join code (pingers-crypto `join_code` over the invite secret and
  the key package), which both phones show at once. Letting one joiner in uses the invite
  up before the server accepts the commit, drops the other requests through it and deletes
  its mailbox, so later joiners are refused at once. Turning someone away sends nothing.
- **Invite posts are read once**: the invite record keeps the last sequence number handed
  in; a post that does not open (anyone who knows the mailbox id can post) is ignored and
  not read again. A withdrawal names the reply mailbox; only the joiner and inviter know
  which request it belongs to (the server sees mailbox ids, so server plus link holder could
  withdraw a request: denial only).
- **Joining without wake-up pushes**: the request carries no subscription, the Add commit
  adds none, and the member's profile names none. Expiry is 24 h or the mailbox's own
  expiry, whichever is sooner; `now >= expires` is expired.
- **Inviter fingerprint** = `account_id(pk)[..8]` (design gives no formula); the joiner refuses
  a Welcome whose group has no such leaf, and keeps nothing.
- **Safety code**: pingers-crypto's `safety_number` (60 digits from 5200 rounds of labelled
  SHA-512 per key, smaller key first), shown in twelve groups of five.
- **Roster history**: the current and previous epochs' rosters are kept (knock verification
  never asks for older ones, design B.4.7).
- **Member rows** exist once a name is known (join request, snapshot or profile operation);
  `roster()` lists every leaf, with `name: None` until then. An added member's name goes to
  the leaf holding their KeyPackage's signature key (`pingers_mls::inspect_key_package`).
- **Profiles** (2026-11-14): `update_profile(&PartyId, ProfileChange::{Name, Subscription},
  rng) -> ProfilePublication::{Send(Box<Outbound>), Waiting}` stores the change at once and
  seals a profile operation (name and subscription id) for the other members, who apply it as
  `Incoming::ProfileUpdated`. While this device's commit, or a leaver's proposal, waits, the
  profile is `Waiting`; `publish_waiting_profile -> Option<Outbound>` sends it once the commit
  has settled (`profile_waiting` tells). pingers-client publishes a new display name to every
  Party and calls `publish_waiting_profile` after each pull and commit.
- **Subscriptions on removal**: every profile, and every member's entry in the state
  snapshot, carries their Party subscription id, so whoever removes a member (including one
  who joined later) lists it in `subscriptions_removed`. Committing a leaver's self-remove
  proposal lists the leaver's subscription too (`pingers_mls::Group::pending_removals`), in
  case the leaver's own `DELETE /subs` did not happen. The server (pingers-delivery) drops
  the listed ids with the commit and ignores unknown ones; remaining members drop the removed
  member's id when they merge the commit.
- **Spots**: last writer wins by Lamport clock, ties broken by the larger wire encoding;
  tombstones stay; a Spot's `created_by` must be its first sender and never changes. Circles
  must be at least `MINIMUM_SPOT_RADIUS`; polygons are checked by `Polygon::new` on create and
  on receipt.
- **Party operations** (profile, Spot) are pingers-proto `PartyOperation`s, padded to the
  smallest mailbox class and sent stored with a Quiet wake (design G.2). The snapshot is a
  pingers-proto `PartySnapshot`. Commits wake Quiet.
- **Settings**: server and suite are fixed at creation; a joiner takes the server from the
  link and the suite from the group it joined (`Group::suite`).
- **Quantum-resistant Parties** (feature `x-wing`, over pingers-mls's): the group runs X-Wing
  (0x004D) and its Meetups use X-Wing grant keys. Without the feature the option fails with
  `UnsupportedSuite` and keeps nothing. pingers-ffi enables it by default; join requests then
  carry an X-Wing key package as well (request 4).
- **Clock injected**; `now` is read once per operation.
- **Per-Party state in store tables** (2026-11-14): settings, roster history, members' subscription
  ids, the in-flight commit and a waiting profile live in pingers-store's schema-v6 tables
  (`party_configs`, `rosters`, `member_subscriptions`, `pending_commits`, `profiles_waiting`),
  which cascade with the Party. Invites, waiting join requests, joins in progress and how this
  device joined a Party live in the schema-v7 tables (2026-11-16: `party_invites`,
  `join_approvals`, which go with their invite, `party_joins` and `joined_via`); this crate keeps
  no key-value records any more. `upgrade_records` moves old records of every kind into the
  tables (one transaction per Party, then one for joins in progress, found with pingers-store's
  `kv_keys_with_prefix`; idempotent; a waiting request whose invite record is gone is dropped);
  pingers-client runs it when it opens the database. A Spot tombstone, own or received, clears
  the Spot's presence row.

## Requests to lower crates

1. **pingers-proto** wire types for Party operations and the snapshot: done (2026-11-14,
   `PartyOperation`, `PartySnapshot`, with vectors and fuzz targets).
2. **pingers-store** tables: done. Settings, roster history, subscriptions, the in-flight commit
   and the waiting profile (schema v6, 2026-11-14); invites, waiting join requests, joins in
   progress and `joined_via` (schema v7, 2026-11-16). A Spot tombstone clears its presence row
   (`Presence::clear`).
3. **pingers-crypto** `safety_number` and `inviter_fingerprint`: done (2026-11-14).
4. **A joiner's suite** (owner decision 2026-11-14): the link keeps the suite out, so a join
   request carries a classical key package and, where the build can make one, an X-Wing one;
   the inviter adds the one its Party's suite needs, and a quantum-resistant Party refuses a
   request without one (`Refusal::NoQuantumResistantKeyPackage`). Done: with the `x-wing`
   feature, which pingers-ffi enables by default, pingers-mls makes X-Wing key packages, so
   every app build fills the slot.
5. **pingers-mls** `inspect_key_package`, `pending_removals`, `Group::suite`, and the X-Wing
   suite (libcrux; two vendored, patched libcrux crates until openmls_libcrux_crypto releases
   on the fixed versions): done (2026-11-14).

## Mutation testing

`cargo mutants -p pingers-parties`: 329 mutants, 232 caught, 91 unviable, 6 missed, all
equivalent: five replace `|` with `^` in the base64url bit packing, where the combined fields
never share a set bit; one replaces `>` with `>=` in `spot_wins` on a branch taken only when
the two Lamport clocks differ.
