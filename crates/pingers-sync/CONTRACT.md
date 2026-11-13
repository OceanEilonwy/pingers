# pingers-sync contract

Talking to the server, reliably, for everyone else (architecture §3, design B.4.4, B.4.8,
B.4.9, B.12, D, E.2). L3: tokio (spawn_blocking, select), no detached tasks.

## API (what pingers-wake, pingers-parties, pingers-meetup-session and pingers-client use)

- `SyncEngine::new(SyncParts { transport, session, clock, timer, rng }, SyncSettings { server,
  role, credentials, token_stock, commit_attempts, queue_capacity })`. `transport` is the
  pingers-transport `Router`; `session` the store session the wake holds.
- `ApiRequest`: a typed builder per route (`Route`, 24 of them). `Route::class()` fixes the
  privacy class, `Route::credential()` the RLN scope of the anonymous credential (writes only).
- `SyncEngine::send(request, Sending { policy, budget }) -> Outcome::{Answered(Response),
  Queued(NotSentReason), Dropped(NotSentReason)}`; `flush_queue(budget) -> FlushReport`;
  `queued()`; `open_live(ApiRequest::live(..)) -> Box<dyn DuplexStream>`.
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
- **Root epoch** = `unix seconds / 3600`, as pingers-quota computes it.
- **Queue** is in memory (32 entries, oldest dropped when full): pingers-store has no
  table for it yet. Reads, board syncs, mailbox registrations and commits are never queued.
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
