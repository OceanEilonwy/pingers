# pingers-wake contract

One background wake, start to finish (design E.2). Implemented in full against the ports below
(2026-11-13); pingers-client wires the ports to pingers-sync, pingers-parties,
pingers-meetup-session and the shell.

## Fixed (other crates build against these)

- `WakeState` (`new`, `restore(WakeSnapshot)`, `snapshot()`, `async run(&WakeEnv, WakeInput,
  rng) -> Result<WakeReport, WakeError>`), `WakeSnapshot` and the stored types it holds
  (`SpotSnapshot`, `LiveLease`, `LeaseSlot`, `RealRecord`, `SentKnock` (with `matched_start`),
  `KnockStatus`, `DecidedKnock`, `KnownEpoch`, `PeerActivity`). The snapshot holds no
  coordinates.
- Inputs: `WakeEnv { device, parties, server, meetups, time }`, `WakeInput { trigger, settings,
  budget }`, `WakeTrigger::{Fix, Geofence(GeofenceEvent), Scheduled, Push(PushReason),
  FocusChanged}`, `WakeSettings`, `MailboxReads`.
- Output: `WakeReport { decision, transitions, upload, matched, knocks_sent, knock_decisions,
  welcomes, notices, next_wake, ending }`, `UploadOutcome`, `KnockOutcome`, `WakeNotice`,
  `WakeEnding::{Completed, OutOfBudget(WakeStep), PortFailed { step, error }}`, `WakeStep`.
- Ports (`ports` module): `DevicePort`, `PartyView` (+ `PartyInfo`, `SpotInfo`), `ServerPort`
  (+ `LeaseUpload`), `MeetupPort` (+ `MeetupAdmission`, `ReplyPost`; `leave(spot)`), and the
  mirror types `FixQuality`, `LocationMode`, `RegionId`, `OsRegion`. Async methods return
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
  read, whether or not its Spot has keys, so a suppressed device reads like any other. This
  includes records held when the wake began that its own upload replaced, so a knock is not
  lost to a re-post, and a knock sent in the same wake counts for the tie-break. A knock is
  decided once (keyed by its reply mailbox).
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
- **`PSK_meet`** (design B.5, wave 4): `MeetupAdmission` and `ReplyPost` carry a
  pingers-presence `MeetupKey` instead of the Spot, epoch and bucket. It is derived from the
  wake's `ActiveSpot` for the record the knock answered, with that record's start bucket as
  `b` (`SentKnock::matched_start` remembers it for the knocker), so the poster and the knocker
  hand pingers-meetup-session the same key whichever bucket they act in. A knocker no longer
  `Inside` the Spot has no keys: it still reads its reply mailbox, but hands nothing on and the
  knock stays `Waiting`. Neither port type is `Clone` or `Debug` any more.
- **Leaving on exit** (design B.5, "Leaving"): a Spot that is `Outside` while a Meetup is live
  there is left through `MeetupPort::leave(spot)` (new), under `WakeStep::LeaveMeetups`;
  suppression still leaves every Meetup with `leave_all`.
- **Slot rotation**: among equal peer activity, Spots without a record in the last lease go
  first.

## Needs from other crates

- pingers-geo: store the Inside confirmation time in `PresenceSnapshot` (review wave 1); the
  restart rule above then becomes a second line of defence.
- pingers-sync: the mailbox pool behind `take_mailbox` and `refill_mailbox_pool`, the previous
  lease's revoke key for `replaces`, and a sealed Meetup subscription for knocks (sent as
  `None` today). Clock skew from `server_time` (F.1) is not handled here yet.
