# pingers-meetup-session contract

A live Meetup on this device: group, live channel, cadence, members, end (architecture §3,
design B.5, B.6, B.6.1, G.4 to G.7).

## Fixed (pingers-client builds against these)

- `Runtime { database, transport, network, clock, timer }` (all `Arc`), `SessionRng`.
- `knock_key_package(&database, suite, rng) -> KnockKeyPackage { key_package, grant_key }`.
- `create(&runtime, CreateMeetup, rng) -> Created { handle, events, welcome }`: post the
  Welcome to the knocker's reply mailbox.
- `join(&runtime, JoinMeetup, rng) -> Joined { handle, events }`.
- `discard_stale(&database) -> usize`: call once at launch.
- `MeetupHandle`: `group`, `view() -> watch::Receiver<MeetupView>`, `screen(MeetupScreen)`,
  `own_fix(Fix)`, `board_seen(segments)`, `act(MeetupAction)`, `add_member(AddMember) ->
  WelcomeBytes`, `leave() -> Ended`, `finished() -> Ended`. Dropping it aborts the task.
- `events: mpsc::UnboundedReceiver<pingers_meetup::MeetupEvent>`: everything except
  `PeerLocation` (positions are in the view).
- `MeetupView { group, own_leaf, cadence, visibility, channel, meet_point, peers }`,
  `position_age(leaf, now)`, `compass(target, &observer, now)`; `PeerView { leaf,
  signature_key, access, holds_own_key, request_pending_until, position }`,
  `PeerView::position_age(now)` ("Updated 40 s ago").
- `MeetupScreen::{Open, Closed}`, `Ended::{Left, AloneTooLong, Removed}`, `ChannelState`.
- `trait GroupNetwork { create_group, post_commit, post_message, fetch_log, delete_group,
  live_head }`, `NetworkError::{Unavailable, Rejected}`.
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
- **Members**: a received proposal (a leaver) is committed at once; `RemoveSilentMember` is a
  remove commit; a lost race (409) discards the commit and reads the log. Every member commit
  uses `Urgency::Quiet` and no snapshot.
- **End**: `leave` sends the self-remove proposal (or, alone, deletes the group on the server);
  `AloneTooLong` deletes the group on the server; `Removed` only locally. Then `Meetups::end`
  (outside a transaction, so the WAL is truncated) deletes rows, chat, keys and MLS state.
- **No resume**: pingers-meetup's session has no snapshot; `discard_stale` deletes what a
  killed process left. Positions are kept only in memory, never in `meetup_peer`.
- **Errors**: only storage failures (and panics) stop the task; anything else drops the input.

## Open (needs other crates)

- pingers-meetup: start grants and asks are ephemeral, so a grant sent before the peer's
  channel is open is lost until the next rotation or re-ask.
- `subscriptions_removed` in remove commits is empty: this crate does not know other members'
  subscription ids.
