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
- **Safety code**: 60 digits, each key's `account_id` read as an integer mod 10³⁰, smaller key
  first. Interim until pingers-crypto has a proper derivation (requests).
- **Roster history**: the current and previous epochs' rosters are kept (knock verification
  never asks for older ones, design B.4.7).
- **Member rows** exist once a name is known (join request, snapshot or profile operation);
  `roster()` lists every leaf, with `name: None` until then.
- **Subscriptions**: the joiner's profile carries its subscription id, so whoever removes a
  member lists it in `subscriptions_removed`. Leavers delete their own.
- **Spots**: last writer wins by Lamport clock, ties broken by the larger encoding; tombstones
  stay; a Spot's `created_by` must be its first sender and never changes. Circles must be at
  least `MINIMUM_SPOT_RADIUS`; polygons are checked by `Polygon::new` on create and on
  receipt.
- **Party operations** (profile, Spot) are padded to the smallest mailbox class and sent
  stored with a Quiet wake (design G.2). Commits wake Quiet.
- **Settings**: server and suite are fixed at creation; a joiner takes the server from the
  link and assumes the classical suite.
- **Clock injected**; `now` is read once per operation.

## Interim parts and requests to lower crates

1. **pingers-proto**: wire types for the Party state operations (`Profile { name,
   subscription }`, `Spot` upsert/tombstone) and the state snapshot (`party_id`, name, member
   profiles, Spots). They are hand-encoded in `src/codec.rs` / `src/ops.rs` for now.
2. **pingers-store**: tables for invites (secret, creation, expiry, status, read position), pending join
   approvals, the in-flight commit, per-Party server/relays/suite, roster history and members'
   subscription ids. Kept meanwhile under `pingers-parties/v1/` through `MlsStorage` on
   `Session` (`src/records.rs`). Also `Presence::delete`, so a Spot tombstone can drop its
   presence row.
3. **pingers-crypto**: a labelled safety-number derivation (`safety_number(pk_a, pk_b)`) and
   an `inviter_fingerprint(pk)`.
4. **A joiner's suite for a quantum-resistant Party.** Flow A keeps the suite out of the link
   (it says something about the Party), so the joiner cannot know it before the Welcome. Open:
   send both a classical and an X-Wing key package, or let the inviter ask for an X-Wing one.
5. **pingers-mls**: the KeyPackage's signature key (to name an added member before merge),
   and `QuantumResistant` once X-Wing is wired.

## Mutation testing

`cargo mutants -p pingers-parties`: 329 mutants, 232 caught, 91 unviable, 6 missed, all
equivalent: five replace `|` with `^` in the base64url bit packing, where the combined fields
never share a set bit; one replaces `>` with `>=` in `spot_wins` on a branch taken only when
the two Lamport clocks differ.
