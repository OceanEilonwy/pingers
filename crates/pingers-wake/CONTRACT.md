# pingers-wake contract

One background wake, start to finish (design E.2). Implemented in full against the ports below
(2026-11-13); pingers-client wires the ports to pingers-sync, pingers-parties,
pingers-meetup-session and the shell.

## Fixed (other crates build against these)

- `WakeState` (`new`, `restore(WakeSnapshot)`, `snapshot()`, `async run(&WakeEnv, WakeInput,
  rng) -> Result<WakeReport, WakeError>`), `WakeSnapshot { spots, last_in_zone, boards,
  epochs, peer_activity }` and the stored types it holds (`SpotSnapshot`, `BoardSnapshot {
  server, leases, sent_knocks, decided_knocks, last_upload }`, `LiveLease`, `LeaseSlot`,
  `RealRecord`, `SentKnock` (with `matched_start`), `KnockStatus`, `DecidedKnock`,
  `KnownEpoch`, `PeerActivity`). The snapshot holds no coordinates.
- Inputs: `WakeEnv { device, parties, server, meetups, time }`, `WakeInput { trigger, settings,
  budget }`, `WakeTrigger::{Fix, Geofence(GeofenceEvent), Scheduled, Push(PushReason),
  FocusChanged}`, `WakeSettings`, `MailboxReads`.
- Output: `WakeReport { decision, transitions, upload, boards, matched, knocks_sent,
  knock_decisions, welcomes, notices, next_wake, ending }`, `BoardReport { server, upload,
  ending }`, `UploadOutcome`, `KnockOutcome`, `WakeNotice`, `WakeEnding::{Completed,
  OutOfBudget(WakeStep), PortFailed { step, error }}`, `WakeStep`.
- Ports (`ports` module): `DevicePort`, `PartyView` (+ `PartyInfo` with its `server`,
  `SpotInfo`), `ServerPort` (every method takes the `&ServerId` first; + `LeaseUpload`),
  `MeetupPort` (+ `MeetupAdmission`, `ReplyPost`; `leave(spot)`), and the mirror types
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
- **One board round per server** (owner decision 2026-11-14: Parties on different servers).
  The device part (gate, fix, zones, presence, geofences, leaving Meetups) runs once. Then each
  server gets a round: pull its group logs, plan the upload from its board's leases and last
  upload, derive keys only for its Parties' Spots, build and upload one lease (its real Spots
  plus dummies, so every board sees one lease per device per bucket), read its board, knock,
  answer knocks on its records, read replies, refill its mailbox pool. Rounds run for every
  server with a Party and every board that still holds a live lease or a `Waiting` knock; a
  board with none of these is dropped from the state. A board with no Party left uploads only
  to withdraw a real record it holds (`ExitWithRealRecord` / `SuppressedWithRealRecord`, all
  dummies), never a cover lease. With no Party at all there is no round and no upload.
- **Round order**: the board whose last successful upload is oldest goes first (never
  uploaded first, ties by server origin), so a server that keeps failing or hanging cannot
  starve the others across wakes.
- **Write-ahead.** A lease is remembered before it is uploaded and a knock before it is
  posted. A wake cut short (budget or network) still skips its own records, answers knocks on
  them and does not knock the same mailbox twice. A successful upload replaces every earlier
  lease on that board (the server revoked them).
- **Budget.** Every port call and fix is raced against the budget on the injected `Timer`
  and dropped when it runs out. Each board round gets `remaining / rounds left` when it
  starts, so time a fast round leaves flows to later ones. After the first timeout or port
  failure in a round no further port call is made in that round; other rounds go on
  (`BoardReport::ending`). If the device part ends early (the routine fix runs out the whole
  budget, or leaving a Meetup fails), every round's network steps are skipped and each
  `BoardReport` carries that ending. The local steps (presence, geofences, location mode, next
  wake) always run, and if anything ended early the next wake comes after `RETRY_AFTER`.
- **Report summary**: `WakeReport::ending` is the device part's ending if it ended early, else
  the first round's (in round order) that did not complete, else `Completed`.
  `WakeReport::upload` is the first round's upload; with no round, `NotDue` at the time of the
  next wake.
- **Quiet rounds send nothing.** A round uses the network only when its upload is due, a
  Commit was pushed (for a board with Parties), or mailboxes are to be read; then that
  server's group logs are pulled first (B.4.7). A suppression with no real record held makes
  no request (B.10).
- **Upload triggers**, per board and from its Parties only, in order: confirmed entry into one
  of its Spots; epoch change (a `GroupCommit` push or a new epoch of one of its Parties,
  re-posted whether or not a real record is held); a held real record that would not be
  posted now (suppressed, or exit); otherwise a cover `Wake`.
- **Mailboxes** are read on a `Mailbox` or `Unknown` push, or on every wake with
  `MailboxReads::EveryWake` ("No wake-up pushes"). Every live real record's knock mailbox is
  read, whether or not its Spot has keys, so a suppressed device reads like any other. This
  includes records held when the wake began that its own upload replaced, so a knock is not
  lost to a re-post, and a knock sent in the same wake counts for the tie-break. Each dummy
  slot's decoy knock mailbox (`LeaseSlot::decoy_mailbox`, taken from the knock pool with the
  lease through `ServerPort::take_decoy_mailbox`, only beyond a reserve kept for real records)
  is read too, in a random order among the real ones, so every lease has
  `Lease::SLOTS` reads whatever number of Spots has records; a decoy's posts are ignored. A
  knock is decided once (keyed by its reply mailbox). After the decisions every mailbox read is
  cleared with `ServerPort::delete_read` (decoys too, so the requests do not show which held
  posts), so handled knocks cannot fill it. Knocks, decisions and unanswered-knock notices
  are kept per board; mailbox reads go to the board's server.
- **Liveness**: one GPS fix per wake with knocks to decide, shared by every round;
  `FreshFix::InsideSpot` only for a fix inside the fence (not the exit band); Focus and the
  gate are read again after the fix.
- **Fixes**: the wake asks for one when the last is a minute old, a Spot is `Candidate` or
  unconfirmed, or sharing resumes; never while suppressed for a reason a fix cannot change.
- **Next wake**: `RETRY_AFTER` if any part ended early; `ENTRY_FIX_SPACING` while a Spot waits
  for a fix; else the rest of `TARGET_UPLOAD_SPACING` since the oldest last upload among the
  boards with Parties (at least `RETRY_AFTER`), so every board gets its upload on time.
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
- pingers-sync: the mailbox pool behind `take_mailbox` and `maintain_push_registrations` (renamed
  from `refill_mailbox_pool` on 2026-11-14: the one registration a wake makes may also renew a
  stale sealed push), the previous
  lease's revoke key for `replaces`, and a sealed Meetup subscription for knocks (sent as
  `None`: a knocker that joins gets its own from the client's maintenance step at the end of the
  joining wake). Clock skew from `server_time` (F.1) is not handled here yet.

## Push priority (added 2026-11-14, v1-push)

- **`PriorityWindow`**, **`PushPriority`**, **`PriorityChange`**: the last 20 high-priority pushes
  and which came downgraded by FCM; 3 of them downgraded raises `Notice::PushDowngraded` (client)
  and marks wake-ups downgraded until the window clears.
- The pipeline's last network step is **`ServerPort::maintain_push_registrations`** (was
  `refill_mailbox_pool`), step **`WakeStep::PushRegistration`** (was `MailboxPool`).
