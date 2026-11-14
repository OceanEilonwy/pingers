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
- Invites (inviter): `create_invite(&PartyId, NewInvite { uses, registration_code }, rng) -> CreatedInvite { invite,
  link, expires }`, `invite_status`, `invites`, `revoke_invite -> DeleteMailbox`,
  `receive_join_request(InviteId, &Padded, rng) -> JoinRequestOutcome::{Admitted,
  AwaitingApproval, Refused(Refusal)}`, `pending_joins`, `approve_join`, `decline_join`.
- Joins (joiner): `prepare_join(&InviteLink, JoinDetails, rng) -> PreparedJoin { join,
  outbound: MailboxPost }`, `accept_welcome(&ArrivedWelcome, rng) -> Joined`.
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

Inputs: posts to an invite mailbox go to `receive_join_request`; the Welcome in a reply
mailbox plus `GET /v1/groups/{id}/state` go to `accept_welcome`; every group log entry goes
to `process_group_message` in log order, except entries this device posted itself (their
`seq` came back from the post): an own commit is already merged by `commit_accepted` and
an own application message cannot be decrypted by its sender (both are errors). pingers-sync keeps the mailbox pool: invites take a
pooled `MailboxPurpose::Invite` mailbox (registered with an Alert sealed push) and joins a
pooled `Reply` one; with none, `NoPooledMailbox`.

## Decisions

- **One commit in flight per Party.** A second is `CommitInFlight`. A rejected Add puts the
  join request back in `pending_joins` (even for a used single-use invite) so it can be
  committed again after catching up.
- **Single-use invites are used up when the request is admitted**, before the server accepts
  the commit, so a second joiner is refused at once. Expiry is 24 h or the mailbox's own
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
  `UnsupportedSuite` and keeps nothing. pingers-ffi does not enable it by default, because a
  joiner cannot yet make an X-Wing KeyPackage (request 4).
- **Clock injected**; `now` is read once per operation.
- **Per-Party state in store tables** (2026-11-14): settings, roster history, members' subscription
  ids, the in-flight commit and a waiting profile live in pingers-store's schema-v5 tables
  (`party_configs`, `rosters`, `member_subscriptions`, `pending_commits`, `profiles_waiting`),
  which cascade with the Party; invites, waiting join requests and joins in progress stay records
  while join flow A settles. `upgrade_records` moves the old records of those five kinds into the
  tables (one transaction per Party, idempotent); pingers-client runs it when it opens the
  database. A Spot tombstone, own or received, clears the Spot's presence row.

## Requests to lower crates

1. **pingers-proto** wire types for Party operations and the snapshot: done (2026-11-14,
   `PartyOperation`, `PartySnapshot`, with vectors and fuzz targets).
2. **pingers-store** tables: done for settings, roster history, subscriptions, the in-flight
   commit and the waiting profile (schema v5); a Spot tombstone clears its presence row
   (`Presence::clear`). Invites, waiting join requests and joins in progress move once join
   flow A has landed on main.
3. **pingers-crypto** `safety_number` and `inviter_fingerprint`: done (2026-11-14).
4. **A joiner's suite for a quantum-resistant Party**: open, owner's decision. Join flow A
   keeps the suite out of the link. The choices are: put the suite in the link; have the
   joiner always send both a classical and an X-Wing KeyPackage; or have the inviter ask
   back for an X-Wing KeyPackage. Until then, a classical KeyPackage sent to an X-Wing Party
   is refused, and nothing changes.
5. **pingers-mls** `inspect_key_package`, `pending_removals`, `Group::suite`, and the X-Wing
   suite (libcrux; two vendored, patched libcrux crates until openmls_libcrux_crypto releases
   on the fixed versions): done (2026-11-14).

## Mutation testing

`cargo mutants -p pingers-parties`: 329 mutants, 232 caught, 91 unviable, 6 missed, all
equivalent: five replace `|` with `^` in the base64url bit packing, where the combined fields
never share a set bit; one replaces `>` with `>=` in `spot_wins` on a branch taken only when
the two Lamport clocks differ.
