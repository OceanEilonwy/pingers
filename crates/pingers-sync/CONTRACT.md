# pingers-sync contract

Talking to the server, reliably, for everyone else (architecture §3, design B.4.4, B.4.8,
B.4.9, B.12, D, E.2). L3: tokio (spawn_blocking, select), no detached tasks.

## API (what pingers-wake, pingers-parties, pingers-meetup-session and pingers-client use)

- `SyncEngine::new(SyncParts { transport, database, clock, timer, rng, self_test }, SyncSettings {
  server, role, credentials, token_stock, commit_attempts, queue_capacity, self_test })`. `transport` is the
  pingers-transport `Router`; `database` the store: each store step opens its own session on the blocking pool, so the
  cross-process lock is held per step and the engine can live for the whole process.
- **One engine per server** (2026-11-14): every store step runs in a pingers-store
  `ServerSession` of `SyncSettings::server`, so an engine reads and writes only its server's
  rows (account standing, bearer, tokens, RLN state, mailboxes, subscriptions, queue, board
  upload); the server must have a `server` row first. The account key in `identity` is the
  device's and is registered on every server; each server records the gateway key hash its
  account was made with (`servers().server_pin`).
  The prover self-test is the device's, not a server's: its result is one device-wide row, and
  the engines of a process share one `SelfTestSlot` (`SyncParts::self_test`), so
  `start_prover_self_test` on a second engine answers `AlreadyRunning` while the first runs.
- Registration refusals: `SyncError::RegistrationCodeRequired` (refused without a code) and
  `SyncError::RegistrationCodeRefused` (refused the code offered); `CodeSource::{Build, Invite,
  User}`.
- `params_direct(&dyn Transport)`: a server's `/v1/params` over the identified (direct) route,
  only to learn the relays of a server the device has not used; everything after it goes
  through the relay.
- `ApiRequest`: a typed builder per route (`Route`, 24 of them). `Route::class()` fixes the
  privacy class, `Route::credential()` the RLN scope of the anonymous credential (writes only).
- `SyncEngine::send(request, Sending { policy, budget }) -> Outcome::{Answered(Response),
  Queued(NotSentReason), Dropped(NotSentReason)}`; `flush_queue(budget) -> FlushReport`;
  `queued()` (async, counts the store's queue); `open_live(ApiRequest::live(group, Option<&sub>, signer)) -> Box<dyn DuplexStream>`
  (chunked OHTTP on the Stream route; pingers-client hands it to pingers-meetup-session's
  `GroupNetwork::open_live`).
- `QueuePolicy::{Drop, KeepUntil(UnixSeconds)}`; `Sending::{or_drop, or_keep_until}`.
- `WakeBudget::{UNLIMITED, from_now(timer, left)}`; `MIN_REQUEST_TIME` (2 s).
- Credentials: `ProcessRole::{MainApp, Extension}`, `CredentialPolicy { accepted,
  prover: ProverStatus }`, `choose_credential(role, policy, RlnReadiness) -> CredentialChoice`,
  `credential_choice()` (for "What your server sees"), `set_credential_policy`.
- Tokens: `TokenStock::{new(minimum, target), design_default}` (20 / 300), `token_count()`,
  `refill_tokens(&TokenIssuer { public_key, key_id }, bearer, budget) -> Refill`.
- Mailboxes: `register_mailbox(&NewMailbox { purpose, lifetime, push: Option<PushSeal> })`,
  `pooled_mailboxes`, `take_mailbox`, `read_mailbox`, `post_to_mailbox`, `delete_mailbox`,
  `forget_expired`. Sealed push: `PushSeal { relay, relay_key, token, urgency }`,
  `new_subscription(seal, lifetime)`, `remember_subscription(group, sub)`.
- Groups: traits `GroupLogOwner { applied_through, log_auth_key, apply }` and `CommitOwner {
  prepare_commit, commit_accepted, commit_rejected }`; `pull_group`, `commit -> Committed`,
  `create_group`, `post_group_message`, `read_group_messages`, `put_subscription`,
  `delete_subscription`, `group_state`, `delete_group`, all addressed by `GroupCall { group,
  key }`. Errors: `GroupError::{Sync, Owner, ConflictsExhausted}`.
- Board: `board_sync` (never queued), `board_page`, `revoke_lease`; `refresh_membership`
  (the RLN leaf list into the local tree).
- For pingers-wake's `ServerPort` (adapters live in pingers-client): `upload_lease(NewLease {
  lease, revoke_key, read })` (replaces the previous upload), `board_page`,
  `post_to_mailbox`, `read_mailbox`, `take_mailbox_for_lease(purpose, lease_start)` (sync),
  `refill_mailbox_pool(&PoolTarget { purpose, size, lifetime })` (at most one registration),
  `knock_subscription(lifetime)` (the knock's Meetup subscription, `None` with pushes off),
  `set_wake_push(Option<WakePush { knock, alert }>)`, `pull_group`.
- For pingers-parties' `Outbound` (adapters in pingers-client): `CreateGroup` ->
  `create_group`; `Commit` -> `commit` through a `CommitOwner` whose `prepare_commit` hands
  out the `OutgoingCommit` and whose `commit_accepted` stashes the Welcome, which the adapter
  then posts with `post_to_mailbox`; `GroupMessage` -> `post_group_message`; `MailboxPost` ->
  `post_to_mailbox`; `DeleteMailbox` -> `delete_mailbox`; `Leave` -> `post_group_message`
  then `delete_subscription`, both with the `GroupCall` key the `LeaveRequest` carries.
  Invites take a pooled `Invite` mailbox and joins a pooled `Reply` one with
  `take_mailbox_until(purpose, until)`.
- Clock (design F.1): `observe_server_time(t) -> ClockStatus::{Unchecked, Corrected,
  Uncorrected, Skewed}` (`ClockOffset`), `corrected_now()`, `clock_status()`, `fetch_params`
  (re-checks the clock). `board_sync` observes the server time it returns.
- `Delivered<T>::{Done, Queued, Dropped}` for writes; `SyncError` per crate.

## Decisions

- **Never direct.** Anonymous and stream requests go only through the router's OHTTP routes.
  `TransportError::NoRelay` (nothing was sent) puts back a taken Privacy Pass token and
  applies the request's policy; an identified request with no relay or no budget fails.
  `OhttpError::KeyMismatch` becomes `SyncError::RelayKeyMismatch` (raise the notice).
- **Credentials are attached at send time**, so a queued request gets one for the bucket it
  is sent in and an unsent one spends none. Main app with an identity whose leaf is live, a
  server accepting proofs and the prover enabled: RLN proof (message id reserved and persisted
  before proving, on the blocking pool); otherwise, or when the bucket's 8 ids are spent, a
  Privacy Pass token. Extensions never prove.
- **Clock skew** follows design F.1: within ±120 s the device clock is corrected; up to half
  a bucket (450 s) it is used unchanged; beyond that every credentialed request fails with
  `SyncError::ClockSkew` (raise `Notice::ClockSkew`) until a server time within range is
  observed. Reads still go.
- **Revoke keys** of the last upload are kept in pingers-store's `board_upload` table (lease id
  and revoke key, one row), not its `lease` table, which wants every slot's Spot and knock
  mailbox that only pingers-wake knows. The first upload after a restart replaces the old
  lease.
- **Pool**: knock mailboxes are sealed with `WakePush::knock`, reply and invite mailboxes and
  knock subscriptions with `WakePush::alert`. `take_mailbox_for_lease` hands out only
  mailboxes that outlive the lease (`start + 4` buckets).
- **Root epoch** = `unix seconds / 3600`, as pingers-quota computes it.
- **Queue** is pingers-store's `request_queue` table (32 entries by default, oldest dropped
  when full), so it survives a restart and is shared by the app's processes. An entry is the
  request's route code and Binary HTTP encoding, without a credential (attached when sent),
  zeroed in memory; a group message also names its group, so ending that Meetup or deleting
  that Party deletes it. An entry is taken off the queue before it is sent: a crash mid-send
  loses it rather than sending it twice. Reads, board syncs, mailbox registrations and
  commits are never queued.
- **Own entries** are not handed back: the positions of this engine's accepted commits and
  stored messages (also those sent from the queue) are kept in pingers-store's
  `own_group_entry` table, and `pull_group` calls `GroupLogOwner::skip_own(sequence)` for them
  instead of `apply`, also after a restart. Positions up to the last entry of each pulled page
  are then forgotten.
- **Epochs** are the owner's: the loop never assumes a group starts at epoch 0 or 1.
- **Commits**: pull, `prepare_commit`, post; on 409 `commit_rejected(head_epoch)`, pull
  again (which applies the winner) and retry, up to `CommitAttempts` (3). The 200 and 409
  bodies are both `CommitResponse`.
- **Wire choices the server must match** (not fixed by `wire.rs` yet): Grp requests carry
  `pingers-group-time: <decimal unix seconds>` and `pingers-group-signature: <hex>` over
  pingers-crypto's `GroupRequest`; group writes (create, commit, post, put subscription) also
  carry an anonymous credential, reads and deletes only the signature; RLN proofs travel as
  `authorization: PingersRln <padded base64url of RlnAuth>`; the first group read is
  `?after=0`, the first mailbox read has no `after`; `DELETE` of an already-forgotten mailbox,
  subscription or group (404) counts as done.

## Tests

`tests/harness` runs against pingers-testkit's `InProcessHarness`. While the server answers
`501` for group routes and `GET /v1/rln/leaves`, the test link answers those from
pingers-delivery and scripted leaf pages; Privacy Pass issuance is answered by pingers-anon's
issuer because the harness server is plain HTTP and `DirectTransport` needs `https://`.

**Surviving mutants (cargo mutants), both unobservable:** `base64url::encode` with `|`
replaced by `^` (the shifted bytes never overlap, so OR and XOR are the same operation); and
`Drop for ApiRequest` replaced with `()` (it zeroizes header values holding bearer tokens and
read keys as the request is freed; no test can read freed memory). Both stay: the first is
the natural way to write it, the second is the style guide's secret hygiene (§8).

## Push lifecycle (added 2026-11-14, v1-push)

- **`maintain_push_registrations`** with **`Maintenance`** `{ pool, groups, subscription_lifetime,
  pushes }` and **`GroupMembership`**: the wake's one push registration. Forgets subscriptions of
  groups not listed (no request), puts a group's missing subscription (or, with `Pushes::Off`, removes
  one) first, then refills an empty pool, else seals the most urgent stale
  registration again in place (`PUT /v1/mailboxes/{id}/push`, `PUT /v1/groups/{id}/subs/{id}`),
  adds a missing group subscription, or clears pushes with **`Pushes::Off`**; with nothing stale it
  tops the pool up. **`Pushes::Unknown`** touches no seal. Answers **`Maintained`**.
- Each mailbox and subscription row records its **`SealStamp`** (pingers-store schema v6): the
  fingerprint of the relay key and token (pingers-crypto's `push_seal_fingerprint`) and the
  seal's `not_after`. **`RENEW_AHEAD`** (three days) is the renewal window; a renewal that would
  gain no time is not made.
- **`push_relay_next_key`** answers **`NextPushRelayKey`** (`NotScheduled`, `Pinned(ScheduledPushRelay)`,
  `Unpinned { activates_at }`) from `GET /v1/push-relays/{id}/config/next`.
- New routes **`Route::PushRelayNextConfig`** (anonymous class, no credential) and
  **`Route::ReplaceMailboxPush`** (anonymous class, Cap: the read key); queue codes 26 and 27.
- `new_subscription` remembers the seal of what it hands out until `remember_subscription`
  records it; a subscription recorded in another process counts as stale and is sealed again.
