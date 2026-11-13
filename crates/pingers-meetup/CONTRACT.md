# pingers-meetup contract

What happens inside a live Meetup, as a sans-IO state machine.

## Fixed (other crates build against these)

- `MeetupSession::start(MeetupConfig, rng)`, `handle(MeetupInput, now, rng) ->
  Vec<MeetupOutput>`, `next_deadline`, `exact_location(leaf)`, `meet_point`,
  `compass(CompassTarget, &Observer, now) -> Option<CompassReading>`.
- Inputs: `MeetupInput::{Received { sender, epoch, message }, MembersChanged { epoch, members },
  OwnFix, Tick, Action(MeetupAction)}`; `MeetupAction::{AskLocation, AnswerRequest,
  RevokeLocation, SendChat, SetMeetPoint, SetVisibility}`; `Member { leaf, signature_key,
  grant_key }`; `MeetupConfig`.
- Outputs: `MeetupOutput::{Send(OutgoingFrame), Event(MeetupEvent), RemoveSilentMember, End}`;
  `OutgoingFrame { message, delivery, wake }`; `MeetupEvent::{Chat, PeerLocation,
  LocationRequested, LocationAccessChanged, MeetPointChanged, BoardEquivocation}`;
  `PeerLocation`; `EndReason`.
- Consent vocabulary: `LocationVisibility::{AskEachTime (default), Everyone, Nobody}`,
  `AlwaysAllow`, `GrantScope`, `AskBack`, `AllowListChoice`, `LocationAnswer`,
  `ExactLocation::{Shared { expires }, Private}`.
- Meet point (implemented): `MeetPointRegister::{new, current, lamport, apply}`,
  `MeetPointUpdate`, `MeetPointDraft::{Fixed, AtMe, Clear}`.
- Compass (implemented): `compass(&Observer, &TargetPosition, now)`, `Observer`,
  `TargetPosition`, `CompassTarget`, `CompassReading`; `coordinate_from_wire` (implemented),
  `coordinate_to_wire`.
- Timers: `KEEP_ALIVE_INTERVAL` (5 min), `SILENCE_LIMIT` (10 min), `ALONE_LIMIT` (30 min),
  `ASK_EXPIRY` (10 min), `SHORT_GRANT` (15 min), `LOCATION_FRAME_INTERVAL` (60 s).
- `MeetupError` variants.

## Still to implement

`MeetupSession` (it holds an uninhabited placeholder field so its methods compile; replace it
with real state) and `coordinate_to_wire`. Property tests for the state machine: frames are
sent at the same cadence and size whatever the grants; no `PeerLocation` or compass reading
for a peer whose key is not held; a revoked peer cannot open frames of the new generation.

## Implementer decides

- Internal state layout, how grants are retried, and which member sends the late-joiner
  `Snapshot` (the lowest online leaf, per design B.5).
- Whether `OwnFix` updates are coalesced between cadence ticks.

## Decisions

- **No dependency on pingers-mls**, although the architecture lists it: the session works on
  decrypted frames and roster values (`Member`, with the grant key read by pingers-mls), and
  emits plaintext frames for pingers-meetup-session to encrypt. That keeps it sans-IO and
  avoids a same-layer dependency.
- **Location frames go out at a fixed 60-second cadence**, real or dummy, with no
  movement-triggered extra sends: design B.6.1's uniform-traffic rule wins over F.3's "send on
  20 m movement", which would reveal movement through timing. The compass-open 2-second rate in
  F.3 is a later, explicit mode, not part of this contract.
- **`BoardSeen`** (added to pingers-proto) carries the segment hashes each member sends first;
  a mismatch raises `MeetupEvent::BoardEquivocation`.
- **Meet point ties** on equal Lamport values go to the higher sender leaf.
- **Leaving** is not a session action: pingers-meetup-session proposes the MLS self-remove and
  deletes the state; the session just ends with the group.
