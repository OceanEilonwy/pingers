# pingers-wake contract

One background wake, start to finish (design E.2). Implemented in full against the ports below
(2026-11-13); pingers-client wires the ports to pingers-sync, pingers-parties,
pingers-meetup-session and the shell.

## Fixed (other crates build against these)

- `WakeState` (`new`, `restore(WakeSnapshot)`, `snapshot()`, `async run(&WakeEnv, WakeInput,
  rng) -> Result<WakeReport, WakeError>`), `WakeSnapshot` and the stored types it holds
  (`SpotSnapshot`, `LiveLease`, `LeaseSlot`, `RealRecord`, `SentKnock`, `KnockStatus`,
  `DecidedKnock`, `KnownEpoch`, `PeerActivity`). The snapshot holds no coordinates.
- Inputs: `WakeEnv { device, parties, server, meetups, time }`, `WakeInput { trigger, settings,
  budget }`, `WakeTrigger::{Fix, Geofence(GeofenceEvent), Scheduled, Push(PushReason),
  FocusChanged}`, `WakeSettings`, `MailboxReads`.
- Output: `WakeReport { decision, transitions, upload, matched, knocks_sent, knock_decisions,
  welcomes, notices, next_wake, ending }`, `UploadOutcome`, `KnockOutcome`, `WakeNotice`,
  `WakeEnding::{Completed, OutOfBudget(WakeStep), PortFailed { step, error }}`, `WakeStep`.
- Ports (`ports` module): `DevicePort`, `PartyView` (+ `PartyInfo`, `SpotInfo`), `ServerPort`
  (+ `LeaseUpload`), `MeetupPort` (+ `MeetupAdmission`, `ReplyPost`), and the mirror types
  `FixQuality`, `LocationMode`, `RegionId`, `OsRegion`. Async methods return
  `impl Future + Send`.
- Constants: `FIX_MAX_AGE` (60 s), `ROUTINE_FIX_TIMEOUT` (15 s), `LIVENESS_FIX_TIMEOUT` (10 s),
  `RETRY_AFTER` (60 s).
- Errors: `WakeError` (local protocol failures only), `PortError` (what ports report).

## Decisions

- **Keys live for one wake.** `ActiveSpot`s are derived inside `run` from `matching_epochs`
  for Spots that are `Inside`, confirmed (below) and not suppressed, and dropped when it
  returns. A Spot that leaves `Inside`, or an epoch past its grace period or after a removal,
  has no keys in the next wake: no matching, and knocks on records of that epoch are not
  opened (the poster must still hold the epoch's keys to answer).
- **Restart.** pingers-geo's `restore` forgets exit evidence and runs the Inside timeout from
  `last_fix_at` (review wave 1). A Spot restored as `Inside` therefore gets no keys and no real
  record until a fix within its exit buffer confirms it, and the wake asks for fixes until
  then. A device that walked away while the process was gone has its real record replaced on
  the first wake (`ExitWithRealRecord`) instead of after up to 60 minutes. An edited fence is
  treated the same way.
- **Write-ahead.** A lease is remembered before it is uploaded and a knock before it is
  posted. A wake cut short (budget or network) still skips its own records, answers knocks on
  them and does not knock the same mailbox twice. A successful upload replaces every earlier
  lease (the server revoked them).
- **Budget.** Every port call and fix is raced against the budget on the injected `Timer`
  and dropped when it runs out. After the first timeout or port failure no further port call
  is made; the local steps (presence, geofences, location mode, next wake) always run, and the
  next wake comes after `RETRY_AFTER`.
- **Quiet wakes send nothing.** The network is used only when an upload is due, a Commit was
  pushed, or mailboxes are to be read; then the group logs are pulled first (B.4.7). A
  suppression with no real record held makes no request (B.10).
- **Upload triggers**, in order: confirmed entry; epoch change (a `GroupCommit` push or a new
  epoch in the Party view, re-posted whether or not a real record is held); a held real record
  that would not be posted now (suppressed, or exit); otherwise a cover `Wake`.
- **Mailboxes** are read on a `Mailbox` or `Unknown` push, or on every wake with
  `MailboxReads::EveryWake` ("No wake-up pushes"). Every live real record's knock mailbox is
  read, whether or not its Spot has keys, so a suppressed device reads like any other. A knock
  is decided once (keyed by its reply mailbox).
- **Liveness**: one GPS fix per wake with knocks to decide; `FreshFix::InsideSpot` only for a
  fix inside the fence (not the exit band); Focus and the gate are read again after the fix.
- **Fixes**: the wake asks for one when the last is a minute old, a Spot is `Candidate` or
  unconfirmed, or sharing resumes; never while suppressed for a reason a fix cannot change.
- **Next wake**: `ENTRY_FIX_SPACING` while a Spot waits for a fix, else the rest of
  `TARGET_UPLOAD_SPACING` since the last upload (at least `RETRY_AFTER`).
- **Location mode**: `Dwell` while a Spot is confirmed Inside, else `Idle`; left alone while a
  Meetup is live.
- **Geofences** are chosen again when the Spots change, a fix leaves the horizon, or the OS
  reports a horizon exit; the selection needs a known position.
- **Slot rotation**: among equal peer activity, Spots without a record in the last lease go
  first.

## Needs from other crates

- pingers-presence: a way to hand `PSK_meet(b)` to pingers-meetup-session without exposing
  `K_spot` (for example `ActiveSpot::meetup_psk(bucket)`); `MeetupAdmission` carries the Spot,
  epoch and bucket until then.
- pingers-geo: store the Inside confirmation time in `PresenceSnapshot` (review wave 1); the
  restart rule above then becomes a second line of defence.
- pingers-sync: the mailbox pool behind `take_mailbox` and `refill_mailbox_pool`, the previous
  lease's revoke key for `replaces`, and a sealed Meetup subscription for knocks (sent as
  `None` today). Clock skew from `server_time` (F.1) is not handled here yet.
