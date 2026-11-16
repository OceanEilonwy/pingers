# pingers-meetup-session contract

A live Meetup on this device: group, live channel, cadence, members, end (architecture §3,
design B.5, B.6, B.6.1, G.4 to G.7).

## Fixed (pingers-client builds against these)

- `Runtime { database, network, clock, timer }` (all `Arc`), `SessionRng`.
- `knock_key_package(&database, Knocking { suite, subscription }, rng) -> KnockKeyPackage {
  key_package, grant_key }`: the subscription id goes in the leaf (`pingers_meetup_subscription`).
- `create(&runtime, CreateMeetup { key, group, knocker, subscriptions, own_subscription,
  board_seen, suite },
  rng) -> Created { handle, events, welcome }`: post the Welcome to the knocker's reply mailbox.
- `join(&runtime, JoinMeetup { key, welcome, grant_key, own_subscription, board_seen }, rng)
  -> Joined {
  handle, events }`. Both futures are `Send`.
- `key` is pingers-presence's `MeetupKey`: `PSK_meet(b)` and its id, from which the Party and
  Spot are taken and the MLS PSK id `(party_id, epoch, spot_id, b)` is built in one place
  (design B.5). Callers can no longer pass a PSK under the wrong id.
- `discard_stale(&database) -> usize`: call once at launch.
- `MeetupHandle`: `group`, `view() -> watch::Receiver<MeetupView>`, `screen(MeetupScreen)`,
  `own_fix(Fix)`, `act(MeetupAction)`, `add_member(AddMember) ->
  WelcomeBytes`, `leave() -> Ended`, `finished() -> Ended`. Dropping it aborts the task.
- `events: mpsc::UnboundedReceiver<pingers_meetup::MeetupEvent>`: every event, positions
  included, each sent after the view that shows it is published. Closed when the task ends.
- `MeetupView { group, own_leaf, cadence, visibility, channel, meet_point, meet_point_set_by,
  requests,
  peers }`,
  `position_age(leaf, now)`, `compass(target, &observer, now)`; `PeerView { leaf,
  signature_key, access, holds_own_key, request_pending, position }`,
  `PeerView::position_age(now)` ("Updated 40 s ago").
- `MeetupScreen::{Open, Closed}`, `Ended::{Left, AloneTooLong, Removed}`, `ChannelState`.
- `trait GroupNetwork { create_group, post_commit, post_message, fetch_log, delete_group,

  open_live }`, `NetworkError::{Unavailable, Rejected}`. `open_live` opens the chunked OHTTP
  live channel through the relay (pingers-sync's `open_live` on the Stream route), its head
  signed with the epoch's log-auth key; the transport is not passed in separately.
- `SessionError`, `NETWORK_TIMEOUT` (30 s), `MAX_RECONNECT_DELAY` (60 s).

## Decisions

- **Group log behind a trait.** pingers-sync (same layer, not a dependency) owns Grp signing,
  Privacy Pass and paging; pingers-client implements `GroupNetwork` over it. Each call takes
  the epoch's `LogAuthKeyPair`.
- **One task per Meetup**, owned by its handle; every store step runs on the blocking pool in
  one transaction with a fresh session (so the cross-process lock is held only per step).
- **Frames**: padded with `Padded::pad_smallest(.., SizeClass::MEETUP_FRAME)`, sealed into
  MLS, carried as `LiveFrame` chunks. Frames with a `wake` (chat, meet point) and stored frames
  while the channel is down go through `post_message`; ephemeral frames with no channel are
  dropped. A frame refused with `CommitPending` (a proposal waits) is dropped.
- **Grants and asks are stored** (pingers-meetup decides; no wake): sent on the channel when
  it is up, where the server stores and forwards them, otherwise posted to the log. A member
  whose channel was down gets them from the log read after it reconnects. What the server
  sees per member event (counts of stored 256 B messages, independent of who holds a key) is
  in pingers-meetup's contract. A copy that arrives both live and from the log (the log is
  read from the last sequence *read*, not the last frame received live) either fails to open
  at the MLS layer or is ignored by the session.
- **Channel**: re-opened at once after a drop, then after 1 s doubling to 60 s; the failure
  count resets only when a frame arrives (a gateway that ends every exchange at once does not
  cause a tight loop). After every (re)open the group log is read from the last sequence seen:
  missed chat and commits, and a newcomer's snapshot. Entries of earlier epochs do not open,
  so a late joiner gets no history.
- **Cadence**: `MeetupScreen::Open` sends `Cadence(Live)`, `Closed` sends `Cadence(Background)`.
  Nothing asks other members to go live.
- **Consent**: `AlwaysAllowAdded(leaf)` writes the member's signature key to `always_allow`;
  chat is written to `meetup_msg`. Visibility comes from the Party override, else the device
  setting; the allow-list from the Party's members on `always_allow`.
- **Members**: a received proposal (a leaver) is committed at once, and again on every tick
  until a commit lands (MLS refuses to send while it waits); `RemoveSilentMember` is a
  remove commit; a lost race (409) discards the commit and reads the log. Every member commit
  uses `Urgency::Quiet` and no snapshot.
- **End**: `leave` sends the self-remove proposal (or, alone, deletes the group on the server);
  `AloneTooLong` deletes the group on the server; `Removed` only locally. Then `Meetups::end`
  (outside a transaction, so the WAL is truncated) deletes rows, chat, keys and MLS state.
- **No resume**: pingers-meetup's session has no snapshot; `discard_stale` deletes what a
  killed process left. Positions are kept only in memory, never in `meetup_peer`.
- **Errors**: only storage failures (and panics) stop the task; anything else drops the input.
- **Subscriptions**: each member's subscription id is in its leaf (`pingers_meetup_subscription`,
  from the knock or the creator's own), so whoever commits a removal (a silent member, a
  leaver's proposal) lists the removed members' subscriptions in `subscriptions_removed` and
  the server stops waking them at once. The last member deletes the whole group instead.
- **Board segments**: the segments a device saw before the Meetup come in with `create` or
  `join` and go out once, as its first frame after the live channel opens (the frame is
  ephemeral); a member whose hashes differ raises `BoardEquivocation` on both sides.

## Read the log on a push (added 2026-11-14, v1-push)

- **`MeetupHandle::read_log`** reads the group log and returns once it is applied: the client
  calls it for a push naming the Meetup's subscription, so a message sent while the live channel
  was down is applied before the wake ends.
- **`meetup_log_auth_key(database, group)`**: the current log-auth key of a Meetup group, for this
  device's own subscription requests outside the session.
