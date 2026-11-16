# pingers-meetup contract

What happens inside a live Meetup, as a sans-IO state machine.

## Fixed (other crates build against these)

- `MeetupSession::start(MeetupConfig, rng)`, `handle(MeetupInput, now, rng) ->
  Vec<MeetupOutput>`, `next_deadline`, `exact_location(leaf)`, `meet_point`,
  `compass(CompassTarget, &Observer, now) -> Option<CompassReading>`; also `shares_with(leaf)`,
  `pending_request(leaf, now)`, `visibility()`, `cadence()`.
- Inputs: `MeetupInput::{Received { sender, epoch, message }, MembersChanged { epoch, members },
  OwnFix, Cadence(CadenceMode), OwnBoardSeen(segments), Tick, Action(MeetupAction)}`;
  `MeetupAction::{AskLocation, AnswerRequest, RevokeLocation, SendChat, SetMeetPoint,
  SetVisibility}`; `Member { leaf, signature_key, grant_key }`; `MeetupConfig`.
- Outputs: `MeetupOutput::{Send(OutgoingFrame), Event(MeetupEvent), RemoveSilentMember, End}`;
  `OutgoingFrame { message, delivery, wake }`; `MeetupEvent::{Chat, PeerLocation,
  LocationRequested, LocationAccessChanged, MeetPointChanged, BoardEquivocation,
  AlwaysAllowAdded(leaf)}`; `PeerLocation`; `EndReason`.
- Consent vocabulary: `LocationVisibility::{AskEachTime (default), Everyone, Nobody}`,
  `AlwaysAllow::{new, contains, insert, members}`, `GrantScope`, `AskBack`, `AllowListChoice`,
  `LocationAnswer`, `ExactLocation::{Shared { expires }, Private}`.
- Cadence: `CadenceMode::{Background (default), Live}`, `BACKGROUND_FRAME_INTERVAL` (60 s),
  `LIVE_FRAME_INTERVAL` (2 s).
- Meet point: `MeetPointRegister::{new, current, lamport, apply}`, `MeetPointUpdate`,
  `MeetPointDraft::{Fixed, AtMe, Clear}`.
- Compass: `compass(&Observer, &TargetPosition, now)`, `Observer`, `TargetPosition`,
  `CompassTarget`, `CompassReading`; `coordinate_from_wire`, `coordinate_to_wire` (rounds half
  away from zero; vectors in `vectors/meetup/`).
- Timers: `KEEP_ALIVE_INTERVAL` (5 min), `SILENCE_LIMIT` (10 min), `ALONE_LIMIT` (30 min),
  `ASK_EXPIRY` (10 min), `SHORT_GRANT` (15 min), `LOCATION_FRAME_INTERVAL` (60 s, the same as
  `BACKGROUND_FRAME_INTERVAL`).
- `MeetupError::{NotAMember, CannotTargetSelf, UnknownRequest, Ended, Overflow, Crypto, Proto,
  Geo}`.

## Still to implement

Nothing in this crate. See "Notes for pingers-meetup-session" for what the caller owns.

## Decisions

- **No dependency on pingers-mls**, although the architecture lists it: the session works on
  decrypted frames and roster values (`Member`, with the grant key read by pingers-mls), and
  emits plaintext frames for pingers-meetup-session to encrypt. That keeps it sans-IO and
  avoids a same-layer dependency.
- **Two fixed cadences, never movement.** `SealedLocation` frames go out one per slot, real or
  dummy: every 60 s in `Background`, every 2 s in `Live` (the Meetup screen or compass is open
  on this device, `MeetupInput::Cadence`). No movement-triggered sends: design B.6.1's
  uniform-traffic rule wins over F.3's "send on 20 m movement". Going live makes a frame due at
  once (or 2 s after the last one); going back waits a full 60 s after the last frame; a late
  tick sends one frame, never a burst. **What the mode switch reveals:** roughly when the app
  is in the foreground on the Meetup screen. The server already sees that from the live
  channel; members see this device's frames speed up, not why or towards whom. `OwnFix` is
  coalesced: the next slot carries the latest fix.
- **Real or dummy is invisible.** A frame is sealed under the current generation when someone
  holds the key and a fix is known; otherwise it is `dummy_location` under the *same* current
  generation (not a fresh one per frame, which would show members the difference). Both are
  80-byte ciphertexts in the 256 B class.
- **Who holds the key is derived, not stored**: visibility, approvals (explicit or allow-list),
  members revoked in this Meetup (excluded from `Everyone` and "meet at me" until approved
  again), and whether the meet point is `AtMember(self)`. `Nobody` beats everything, including
  "meet at me". After every change the session reconciles: anyone who lost access forces a
  rotation (generation + 1, fresh key, grants to everyone who keeps access).
- **Uniform grant traffic.** A rotation, a visibility change and `start` always send exactly
  one grant-sized frame per other member (real grants, or dummies sealed to a throwaway grant
  key of the same suite as that member's real one, `GrantKeyPair::generate_like`, so they have
  the real structure and length); a membership change sends one per
  newcomer, real under `Everyone`, dummy otherwise. **Any** member leaving rotates, so a
  rotation does not show whether the leaver held the key; a revoke always rotates, even of a
  non-holder. Residual leaks: members see that an ask was sent (not by whom to whom; the MLS
  sender is visible to members, not the server), and that a single grant-sized frame followed
  or did not ("Not now" is silent, by design); generation changes show that some rotation
  happened.
- **Grants and asks are stored** in the group log (`Delivery::Stored`, no wake), like chat and
  the meet point, so a member whose live channel was down when one was sent gets it when it
  reads the log after reconnecting (an ephemeral grant was simply lost: the member who
  approved believed they were sharing while the requester never got the key; after a
  rotation, likewise). They stay HPKE-sealed to the recipient's grant key with no recipient
  field, padded to the 256 B class (4 KiB with X-Wing grant keys); dummy grants are stored
  exactly like real ones.
- **What the server sees, per member event** (stored 256 B messages with no wake, in the log
  and on the channels): `start` N−1 (N members); a rotation or a setting change N−1; a
  membership change one per newcomer, or N−1 (newcomers included) if anyone left, which
  rotates; an ask 1; an
  "Allow" 1 (an automatic re-grant to a holder who asks again, 1); "Not now" 0. Counts depend
  only on the event and the group size, never on the setting or on who holds the key. These
  are exactly the off-cadence 256 B frames the server already saw when grants were ephemeral:
  same events, counts, sizes and timing. What changes: (1) they are marked stored rather than
  ephemeral, so they no longer blend with board-hash frames (`BoardSeen`, still ephemeral and
  sent at protocol-fixed moments the server can already place); (2) they stay in the log
  until the Meetup ends (MLS-encrypted, deleted with the group); (3) one sent while this
  device's channel is down is posted through the log rather than dropped, so the count shows
  where it used to vanish, still independent of grantees. None of this tells the server or
  members whether, or with whom, a member shares. They never wake anyone: a push would show
  that a real grant or ask was aimed at an absent member.
- **Delivered twice, or late.** A copy read from the log after the live one changes nothing:
  within one generation the later end wins in either order (a sender only ever extends an end
  within a generation); a grant of a generation older than the newest this device has seen
  from that sender, in a frame or a grant, is ignored, even when no key is held (so a member
  rotated away does not get an old key back by replaying the log).
- **"Until the Meetup ends"** is written as `expires_at = u64::MAX` in the grant plaintext and
  read back as `ExactLocation::Shared { expires: None }`.
- **Asks** are accepted only if the requester inside equals the MLS sender and `asked_at` is
  less than `ASK_EXPIRY` old, also when read late from the log. A request id is acted on once
  (prompt, automatic grant or drop): ids are remembered until `asked_at + ASK_EXPIRY`, whatever
  the answer, so a copy from the log never prompts again or triggers another grant. A member
  who already holds the key and asks again (a new id) is re-sent its grant without a prompt.
  A pending request lapses `ASK_EXPIRY` after `asked_at` (after receipt if the sender's clock
  is ahead); expired requests are refused when answered (`UnknownRequest`) and purged on any
  tick. "Not now" returns no outputs and keeps nothing but the request id until it expires,
  the same as an unanswered request.
- **Grants** are trial-opened with the grant context of the MLS sender; a grant for an older
  generation than the newest seen is ignored (see "Delivered twice, or late"); a frame of a newer generation than the one held
  means this device was rotated away (`LocationAccessChanged(Private)`). A held key past its
  end gives no compass even before the next tick.
- **`BoardSeen`** (added to pingers-proto) carries the segment hashes each member sends first;
  a mismatch raises `MeetupEvent::BoardEquivocation`, on whichever side learns second.
- **Meet point ties** on equal Lamport values go to the higher sender leaf. `AtMember` is
  accepted only from that leaf, also inside a `Snapshot`; its owner re-sends it to newcomers.
- **Late joiners**: the lowest pre-existing leaf sends `Snapshot { meet_point, lamport }`
  (`Cleared { lamport }` is applied when there is no meet point, so the newcomer's clock
  catches up). Thread heads are not in the proto `Snapshot`, so a reply to an unknown message
  keeps its parent as given.
- **Keep-alives** are subsumed by the cadence (at most 60 s between frames, well inside 5 min),
  so the session never emits `KeepAlive`; any received frame counts as a sign of life.
- **Silent members**: `RemoveSilentMember` once `SILENCE_LIMIT` passes without a frame, then
  again one limit later if the member is still there (every member may ask; the group log
  serialises the commits).
- **Leaving** is not a session action: pingers-meetup-session proposes the MLS self-remove and
  deletes the state; the session just ends with the group.
- **X-Wing is off** (MLS X-Wing is parked over upstream advisories), so grants and asks use the
  classical X25519 HPKE suite and fit the 256 B class. An X-Wing grant or ask (1,120-byte
  encapsulation plus its plaintext and tag, about 1,180 bytes) pads to the 4 KiB class instead,
  and a dummy grant to an X-Wing member is sealed to a throwaway X-Wing key, so in a
  quantum-resistant Meetup every grant and ask, real or dummy, is a 4 KiB frame (tests
  `a_quantum_resistant_meetup_sends_the_same_grant_frames_sharing_or_not` and
  `asks_in_a_quantum_resistant_meetup_are_indistinguishable`).

## Notes for pingers-meetup-session

- Call `handle(Tick)` at `next_deadline()` (it changes after every input), and drain the
  outputs of `start` with the first `handle`.
- Pad every `Send` frame to its `SizeClass::MEETUP_FRAME` class before MLS encryption; location,
  grant, ask and board frames are all 256 B.
- Honour `delivery`: stored frames (grants and asks included) must reach the group log even
  when the live channel is down, and the log must be read after every reconnect.
- Send `Cadence(Live)` when the Meetup screen or compass opens and `Cadence(Background)` when
  it closes or the app backgrounds. A viewer's compass only gets 2 s updates from peers that
  are themselves live; making peers follow a viewer would need a signal that reveals who is
  looking, so it is not done.
- Persist `AlwaysAllowAdded(leaf)` (map the leaf to its signature key) to `always_allow`.
- On `End`, delete the group, keys and content; the session is unusable afterwards.
