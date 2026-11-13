# pingers-presence contract

The co-presence protocol, sans-IO. Client half by default; server half behind `server`.
Implemented in full (2026-11-13).

## Fixed (other crates build against these)

- Spot keys: `ActiveSpot::derive(&PresenceRoot, &SpotStatus) -> Result<ActiveSpot, _>`
  (fails with `NotInside` unless the state is `Inside`), `ActiveSpot::{spot, epoch}`,
  `SpotRef { party, spot }`, `SpotStatus { spot, epoch, state }`.
- Leases: `choose_slots`, `build_lease(&LeaseInputs, rng) -> BuiltLease { lease, slots }`,
  `RealSlot`, `SlotCandidate`, `SlotRecord`, `SlotContent`, `LEASE_BUCKETS` (4).
- Matching: `TrialMatcher::new(&[ActiveSpot], now: Bucket)`, `match_page(&BoardPage,
  &OwnRecords) -> Vec<BoardMatch>`, `knock_targets(&[BoardMatch], &KnockFilter)` (at most
  `MAX_KNOCKS_PER_ARRIVAL` = 3, Meetup hints first), `KnockFilter { already_knocked,
  own_meetups }`, `OwnRecords`, `BoardMatch`.
- Epoch lag: `matching_epochs(&EpochState, now) -> MatchingEpochs { current, previous }`,
  `EpochState { current, entered_at, transition }`, `EpochTransition::{Removal, Other}`.
- Knocks: `build_knock(&KnockInputs, rng) -> OutgoingKnock { mailbox, post }`,
  `open_knock(&KnockContext, post) -> OpenedKnock`, `verify_knock(&KnockContext, OpenedKnock,
  &impl RosterLookup) -> VerifiedKnock`, `trait RosterLookup { signature_key(leaf, epoch),
  current_epoch() }` (pingers-parties implements it).
- Liveness: `decide_knock(&VerifiedKnock, &LivenessEvidence, &KnockSituation) ->
  KnockDecision::{Accept(Admission), Decline, WaitForPeer}`, `FreshFix`, `Sharing`,
  `Admission::{CreateMeetup, AddToMeetup}`.
- Mailboxes: `prepare_mailbox(&MailboxRequest, rng) -> MailboxRegistration { read_key, spec }`,
  `MailboxPurpose`, `PushSealing`, `knock_unanswered`, `mailbox_expires_at`,
  `mailbox_covers_lease`, `LEASE_DURATION`, `MAX_MAILBOX_LIFETIME` (30 days).
- Cover policy: `plan_upload(UploadTrigger, last_upload, now) -> UploadPlan`,
  `read_board(UploadTrigger, &ReadPolicy, rng) -> ReadBoard`, `UploadTrigger` (with
  `EpochChanged`), `DataMode`, `EpochChange`, `MIN_UPLOAD_SPACING` (20 min),
  `TARGET_UPLOAD_SPACING` (45 min).
- Server (`server`): `check_lease`, `segment_expired`, `live_segments`, `segment_closed`,
  `closed_segment_hash`, `check_mailbox_spec`, `check_mailbox_live`, `check_mailbox_post`.
  Segment hashes are `pingers_crypto::segment_hash`.
- `PresenceError` variants.
- Vectors: `vectors/presence/{lease,trial-match,knock}.json`.

## Implementer decides

- How `TrialMatcher` precomputes per-Spot state for speed. Today it calls
  `pingers_crypto::tag_matches` per record and Spot and derives the seal key only on a tag
  match; any speed-up belongs in pingers-crypto's `TagKey`.

## Decisions

- **No dependency on pingers-gate or pingers-mls** (same layer). The gate's answer arrives as
  `Sharing`; roster lookups go through `RosterLookup`.
- **Keys derivable only when Inside**: enforced by `ActiveSpot::derive`, the only way to get
  Spot keys in this crate (design B.4.3 client rule).
- **Tie-break**: when both devices knocked each other, the device whose record randomness is
  smaller (bytewise) creates the group; the other answers `WaitForPeer`.
- **Slot choice**: most recent peer activity first; never-active Spots keep the caller's order,
  so the caller rotates by reordering. The design says "rotates the rest in on refresh"
  without a rule.
- **Server lease check** accepts the current bucket or the previous one, to tolerate the clock
  skew the design allows (±120 s, F.1). It also refuses a lease whose records repeat a
  randomness (`DuplicateRecord`).
- **Low data mode** reads on confirmed entry and after any Party epoch change; non-entry
  uploads in normal mode read with probability `cover_read_ratio`, including exits.
- **Shuffle and randomness order** (so vectors can list the bytes): per slot in order, 16
  bytes of `r` for a real slot or 96 bytes for a dummy; then Fisher–Yates for i = 3, 2, 1,
  each draw 4 bytes big-endian, rejected while at or above the largest multiple of i + 1 not
  above 2³², j = draw mod (i + 1). `prepare_mailbox` draws the read key, then the 8-byte
  push handle, then the HPKE randomness.
- **`TrialMatcher::new` takes `now`** (changed from the stub): design A.2 says readers reject
  records older than 4 buckets, which needs the reader's own clock. Segments that have
  expired, or start more than one bucket ahead, are skipped; a record repeated within a page
  is reported once. A tag match whose seal does not open (or whose beacon has bad reserved
  bytes) is no match.
- **The knock's `party_epoch` is the epoch of the keys it matched under**, not necessarily the
  knocker's newest epoch. The poster opens it with that epoch's keys, so `verify_knock`
  refuses any other epoch (`WrongEpoch`) and checks the roster at that epoch.
- **Removed members** (design A.3): `RosterLookup` gains `current_epoch()`, and
  `verify_knock` also requires the knocker's leaf to hold the same key at the current epoch.
  A member removed since the knock's epoch, or whose leaf was reused, gets `UnknownKnocker`.
- **Epoch grace** (design B.4.7): `matching_epochs` returns the previous epoch for one lease
  (60 min) after a Commit that was not a removal, and never after a removal. "One lease
  horizon" is read as `LEASE_DURATION` from when this device processed the Commit.
- **`UploadTrigger::EpochChanged`** uploads at once, ignoring spacing. Every member woken by a
  Commit re-posts (design B.4.7), whether or not it holds a real record, so post-Commit
  uploads do not reveal who is in a Spot.
- **`knock_targets` takes a `KnockFilter`** (changed from the stub): mailboxes already knocked
  are skipped (a refreshed record names a new mailbox, so re-knocking per design B.4.4 step 7
  still happens), and records hinting at a Meetup the device is already in are skipped, so
  Meetup members do not knock each other on every sync.
- **`build_knock` checks its keys** against the match (`SpotMismatch`).
- **Mailbox lifetimes**: `prepare_mailbox` refuses lifetimes over 30 days (`MailboxRule`), as
  the server would. `MAX_MAILBOX_LIFETIME` moved out of the `server` feature so the client
  can use it. `mailbox_covers_lease` says whether a pooled mailbox outlives a lease starting
  in a given bucket; the design gives knock and reply mailboxes "one lease" without saying
  from when, so the pool must register them with enough margin.
- **Segments close one bucket late**: because `check_lease` accepts the previous bucket, a
  segment takes appends during its own bucket and the next; `segment_closed` and
  `closed_segment_hash` treat it as closed from bucket `start + 2`.
- **New `PresenceError` variants**: `SpotMismatch`, `WrongEpoch`, `DuplicateRecord`,
  `Internal`. `NotImplemented` is removed.
