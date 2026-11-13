# pingers-presence contract

The co-presence protocol, sans-IO. Client half by default; server half behind `server`.

## Fixed (other crates build against these)

- Spot keys: `ActiveSpot::derive(&PresenceRoot, &SpotStatus) -> Result<ActiveSpot, _>`
  (fails with `NotInside` unless the state is `Inside`), `ActiveSpot::{spot, epoch}`,
  `SpotRef { party, spot }`, `SpotStatus { spot, epoch, state }`.
- Leases: `choose_slots` (implemented), `build_lease(&LeaseInputs, rng) -> BuiltLease
  { lease, slots }`, `RealSlot`, `SlotCandidate`, `SlotRecord`, `SlotContent`,
  `LEASE_BUCKETS` (4).
- Matching: `TrialMatcher::new(&[ActiveSpot])`, `match_page(&BoardPage, &OwnRecords) ->
  Vec<BoardMatch>`, `knock_targets` (implemented, at most `MAX_KNOCKS_PER_ARRIVAL` = 3,
  Meetup hints first), `OwnRecords`, `BoardMatch`.
- Knocks: `build_knock(&KnockInputs, rng) -> OutgoingKnock { mailbox, post }`,
  `open_knock(&KnockContext, post) -> OpenedKnock`, `verify_knock(&KnockContext, OpenedKnock,
  &impl RosterLookup) -> VerifiedKnock`, `trait RosterLookup` (pingers-parties implements it).
- Liveness: `decide_knock(&VerifiedKnock, &LivenessEvidence, &KnockSituation) ->
  KnockDecision::{Accept(Admission), Decline, WaitForPeer}` (implemented), `FreshFix`,
  `Sharing`, `Admission::{CreateMeetup, AddToMeetup}`.
- Mailboxes: `prepare_mailbox(&MailboxRequest, rng) -> MailboxRegistration { read_key, spec }`,
  `MailboxPurpose`, `PushSealing`, `knock_unanswered` (implemented), `LEASE_DURATION`.
- Cover policy (implemented): `plan_upload(UploadTrigger, last_upload, now) -> UploadPlan`,
  `read_board(UploadTrigger, &ReadPolicy, rng) -> ReadBoard`, `DataMode`, `EpochChange`,
  `MIN_UPLOAD_SPACING` (20 min), `TARGET_UPLOAD_SPACING` (45 min).
- Server (`server`, implemented): `check_lease`, `segment_expired`, `live_segments`,
  `check_mailbox_spec`, `check_mailbox_post`, `MAX_MAILBOX_LIFETIME` (30 days). Segment hashes
  are `pingers_crypto::segment_hash`.
- `PresenceError` variants.

## Still to implement

`ActiveSpot::derive`, `build_lease`, `TrialMatcher::match_page`, `build_knock`, `open_knock`,
the signature step of `verify_knock`, `prepare_mailbox`; plus property tests (a lease always
has four records; matching finds every real record of an active Spot and nothing else) and
tests for `read_board` with a seeded RNG.

## Implementer decides

- How `TrialMatcher` precomputes per-Spot state for speed (it is the hot loop: up to 8 HMACs
  per record).
- The shuffle algorithm (any uniform shuffle driven by the injected RNG).

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
  skew the design allows (±120 s, F.1).
- **Low data mode** reads on confirmed entry and after any Party epoch change; non-entry
  uploads in normal mode read with probability `cover_read_ratio`, including exits.
