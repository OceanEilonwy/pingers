// SPDX-License-Identifier: AGPL-3.0-only
//! The Meetup session state machine (design B.5, B.6, B.6.1).

use core::time::Duration;

use pingers_crypto::{AccountPublicKey, GrantPublicKey, GrantSecretKey};
use pingers_geo::{Bearing, Coordinate, Fix, Meters};
use pingers_proto::{
    ChatBody, ChatMessageId, Delivery, Epoch, GroupId, LeafIndex, LocationRequestId, MeetPoint,
    MeetupMessage, Urgency,
};
use pingers_time::UnixSeconds;
use rand_core::CryptoRng;

use crate::stub::{Unimplemented, not_implemented};
use crate::{
    AlwaysAllow, CompassReading, CompassTarget, ExactLocation, LocationAnswer, LocationVisibility,
    MeetPointDraft, MeetupError, Observer,
};

/// A member sends a location or keep-alive frame at least this often (design B.5).
pub const KEEP_ALIVE_INTERVAL: Duration = Duration::from_mins(5);

/// A member silent this long is removed by any other member (design B.5).
pub const SILENCE_LIMIT: Duration = Duration::from_mins(10);

/// A lone member ends the Meetup after this long alone (design B.5).
pub const ALONE_LIMIT: Duration = Duration::from_mins(30);

/// An unanswered location request expires after this long (design B.6.1).
pub const ASK_EXPIRY: Duration = Duration::from_mins(10);

/// The short grant scope (design B.6.1).
pub const SHORT_GRANT: Duration = Duration::from_mins(15);

/// The fixed cadence of `SealedLocation` frames, real or dummy (design B.6.1, F.3).
pub const LOCATION_FRAME_INTERVAL: Duration = Duration::from_secs(60);

/// A Meetup member as the MLS group reports it after a commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    /// Its leaf in the Meetup group.
    pub leaf: LeafIndex,
    /// Its signature key (the same as in the Party roster).
    pub signature_key: AccountPublicKey,
    /// Its grant key from the `pingers_grant_key` leaf extension.
    pub grant_key: GrantPublicKey,
}

/// What a session starts from.
pub struct MeetupConfig {
    /// The Meetup group.
    pub group: GroupId,
    /// This device's leaf in it.
    pub own_leaf: LeafIndex,
    /// The epoch at joining.
    pub epoch: Epoch,
    /// This device's grant secret (its public half is in its leaf extension).
    pub own_grant_key: GrantSecretKey,
    /// The members at joining, including this device.
    pub members: Vec<Member>,
    /// The visibility setting in force for this Party.
    pub visibility: LocationVisibility,
    /// The Party's local allow-list.
    pub always_allow: AlwaysAllow,
    /// When this device joined.
    pub joined_at: UnixSeconds,
}

/// Something the user did in the Meetup.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum MeetupAction {
    /// "Ask to see" a member.
    AskLocation(LeafIndex),
    /// Answer an incoming request.
    AnswerRequest {
        /// The request.
        request: LocationRequestId,
        /// The answer.
        answer: LocationAnswer,
    },
    /// Stop sharing with a member (rotates the location key).
    RevokeLocation(LeafIndex),
    /// Send a chat message.
    SendChat {
        /// The text.
        body: ChatBody,
        /// The thread root it replies to.
        parent: Option<ChatMessageId>,
    },
    /// Set or clear the meet point.
    SetMeetPoint(MeetPointDraft),
    /// Change the visibility setting (to `Nobody` rotates the key).
    SetVisibility(LocationVisibility),
}

/// An input to the session. Every call also passes the current time.
#[derive(Debug)]
#[non_exhaustive]
pub enum MeetupInput {
    /// A decrypted, decoded Meetup frame from the group.
    Received {
        /// The sender's leaf (from MLS).
        sender: LeafIndex,
        /// The epoch it was sent in.
        epoch: Epoch,
        /// The frame.
        message: MeetupMessage,
    },
    /// The group moved to a new epoch with these members (after a commit).
    MembersChanged {
        /// The new epoch.
        epoch: Epoch,
        /// All members now, including this device.
        members: Vec<Member>,
    },
    /// A new fix of this device's position.
    OwnFix(Fix),
    /// Time passed; send due frames and apply timers.
    Tick,
    /// A user action.
    Action(MeetupAction),
}

/// A frame to encrypt into the group and send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutgoingFrame {
    /// The frame (pad it to its size class before MLS encryption).
    pub message: MeetupMessage,
    /// Stored (chat, meet point) or ephemeral (location, keep-alive, grants, asks).
    pub delivery: Delivery,
    /// Whether to wake members whose live channel is down.
    pub wake: Option<Urgency>,
}

/// A peer's decrypted position. Only produced for peers whose location key is held.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PeerLocation {
    /// The peer.
    pub leaf: LeafIndex,
    /// Where.
    pub position: Coordinate,
    /// How accurate.
    pub accuracy: Meters,
    /// Which way they face, if known.
    pub heading: Option<Bearing>,
    /// When the fix was taken.
    pub taken_at: UnixSeconds,
}

/// Something the shell or core should know about.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum MeetupEvent {
    /// A chat message arrived.
    Chat {
        /// Its id.
        id: ChatMessageId,
        /// Its thread root, if a reply.
        parent: Option<ChatMessageId>,
        /// Who sent it.
        sender: LeafIndex,
        /// The text.
        body: ChatBody,
        /// When it was written.
        written_at: UnixSeconds,
    },
    /// A peer's position updated.
    PeerLocation(PeerLocation),
    /// A member asks to see this device's location (`AskEachTime` only).
    LocationRequested {
        /// Who.
        from: LeafIndex,
        /// The request, for the answer.
        request: LocationRequestId,
    },
    /// Whether this device can see a peer changed (granted, expired, rotated away).
    LocationAccessChanged {
        /// The peer.
        peer: LeafIndex,
        /// The new access.
        access: ExactLocation,
    },
    /// The meet point changed.
    MeetPointChanged,
    /// A member's board segment hashes differ from ours (design B.4.3).
    BoardEquivocation,
}

/// Why the session ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EndReason {
    /// This device was alone for [`ALONE_LIMIT`].
    AloneTooLong,
    /// This device was removed from the group.
    Removed,
}

/// What the caller must do.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum MeetupOutput {
    /// Encrypt and send a frame.
    Send(OutgoingFrame),
    /// Tell the shell.
    Event(MeetupEvent),
    /// Commit removal of a member silent for [`SILENCE_LIMIT`].
    RemoveSilentMember(LeafIndex),
    /// The Meetup is over for this device: delete the group, keys and content.
    End(EndReason),
}

/// One device's view of a live Meetup.
pub struct MeetupSession {
    unimplemented: Unimplemented,
}

impl MeetupSession {
    /// Starts a session: generates the first location key, and under `Everyone` prepares
    /// grants for the current members (returned by the first [`MeetupSession::handle`]).
    ///
    /// # Errors
    ///
    /// [`MeetupError::NotAMember`] if `own_leaf` is not among the members.
    pub fn start(config: MeetupConfig, rng: &mut impl CryptoRng) -> Result<Self, MeetupError> {
        not_implemented((config, rng))
    }

    /// Processes one input at time `now`.
    ///
    /// # Errors
    ///
    /// [`MeetupError::Ended`] after an [`MeetupOutput::End`]; [`MeetupError::NotAMember`] for an
    /// action naming a non-member; [`MeetupError::UnknownRequest`]; [`MeetupError::Crypto`] for
    /// internal failures (failed trial-opens of grants and asks are not errors).
    pub fn handle(
        &mut self,
        input: MeetupInput,
        now: UnixSeconds,
        rng: &mut impl CryptoRng,
    ) -> Result<Vec<MeetupOutput>, MeetupError> {
        let _ = (input, now, rng);
        match self.unimplemented {}
    }

    /// When the next timer falls due (frame cadence, keep-alive, silence, alone, expiries);
    /// the caller sends [`MeetupInput::Tick`] then.
    #[must_use]
    pub fn next_deadline(&self) -> Option<UnixSeconds> {
        match self.unimplemented {}
    }

    /// Whether this device can see `peer`.
    #[must_use]
    pub fn exact_location(&self, peer: LeafIndex) -> ExactLocation {
        let _ = peer;
        match self.unimplemented {}
    }

    /// The current meet point.
    #[must_use]
    pub fn meet_point(&self) -> Option<&MeetPoint> {
        match self.unimplemented {}
    }

    /// The compass towards `target`, or `None` if the target's position is not decryptable
    /// (design B.6.1: never computed for a member whose key is not held).
    #[must_use]
    pub fn compass(
        &self,
        target: CompassTarget,
        observer: &Observer,
        now: UnixSeconds,
    ) -> Option<CompassReading> {
        let _ = (target, observer, now);
        match self.unimplemented {}
    }
}
