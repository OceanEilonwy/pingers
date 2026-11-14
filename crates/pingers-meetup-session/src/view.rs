// SPDX-License-Identifier: AGPL-3.0-only
//! The read-only snapshot of a live Meetup that pingers-client publishes to pingers-screens.

use core::time::Duration;

use pingers_crypto::AccountPublicKey;
use pingers_meetup::{
    CadenceMode, CompassReading, CompassTarget, ExactLocation, LocationVisibility, Observer,
    PeerLocation, TargetPosition, compass, coordinate_from_wire,
};
use pingers_proto::{GroupId, LeafIndex, MeetPoint};
use pingers_time::UnixSeconds;

/// Whether the live channel is up.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ChannelState {
    /// Frames flow both ways.
    Connected,
    /// The channel dropped or could not be opened; it is being re-opened. Location frames
    /// are not delivered meanwhile; chat and the meet point go through the group log.
    Reconnecting,
}

/// One other member, as the Meetup and member rows show them.
#[derive(Clone, Debug, PartialEq)]
pub struct PeerView {
    /// Its leaf in the Meetup group.
    pub leaf: LeafIndex,
    /// Its account key, to find its name and avatar in the Party.
    pub signature_key: AccountPublicKey,
    /// Whether this device can see it (design B.6.1).
    pub access: ExactLocation,
    /// Whether it holds this device's current location key ("Stop sharing").
    pub holds_own_key: bool,
    /// When this device's request to see it lapses, while one is pending ("Request sent").
    pub request_pending_until: Option<UnixSeconds>,
    /// Its latest decrypted position; only while its key is held.
    pub position: Option<PeerLocation>,
}

impl PeerView {
    /// How old its latest position is at `now` ("Updated 40 s ago"), or `None` if no
    /// position is known or its key is not held.
    #[must_use]
    pub fn position_age(&self, now: UnixSeconds) -> Option<Duration> {
        self.position
            .map(|position| now.saturating_duration_since(position.taken_at))
    }
}

/// A live Meetup as the screens show it.
#[derive(Clone, Debug, PartialEq)]
pub struct MeetupView {
    /// The Meetup group.
    pub group: GroupId,
    /// This device's leaf.
    pub own_leaf: LeafIndex,
    /// The location-frame cadence in force on this device.
    pub cadence: CadenceMode,
    /// The visibility setting in force.
    pub visibility: LocationVisibility,
    /// The live channel.
    pub channel: ChannelState,
    /// The current meet point.
    pub meet_point: Option<MeetPoint>,
    /// Who set it, while there is one.
    pub meet_point_set_by: Option<LeafIndex>,
    /// The other members, in leaf order.
    pub peers: Vec<PeerView>,
}

impl MeetupView {
    /// The member at `leaf`.
    #[must_use]
    pub fn peer(&self, leaf: LeafIndex) -> Option<&PeerView> {
        self.peers.iter().find(|peer| peer.leaf == leaf)
    }

    /// How old `leaf`'s latest position is at `now`: the age the compass and member rows show.
    #[must_use]
    pub fn position_age(&self, leaf: LeafIndex, now: UnixSeconds) -> Option<Duration> {
        self.peer(leaf)?.position_age(now)
    }

    /// The compass towards `target`, or `None` if its position is not decryptable (never
    /// computed for a member whose key is not held, design B.6.1) or not yet known.
    /// [`CompassReading::age`] is how old the target's position is.
    #[must_use]
    pub fn compass(
        &self,
        target: CompassTarget,
        observer: &Observer,
        now: UnixSeconds,
    ) -> Option<CompassReading> {
        let position = match target {
            CompassTarget::Member(leaf) => self.peer_position(leaf)?,
            CompassTarget::MeetPoint => match self.meet_point.as_ref()? {
                MeetPoint::Fixed { position, .. } => TargetPosition {
                    position: coordinate_from_wire(*position).ok()?,
                    taken_at: now,
                },
                MeetPoint::AtMember { leaf, .. } => self.peer_position(*leaf)?,
                MeetPoint::Cleared { .. } => return None,
            },
        };
        Some(compass(observer, &position, now))
    }

    fn peer_position(&self, leaf: LeafIndex) -> Option<TargetPosition> {
        let position = self.peer(leaf)?.position?;
        Some(TargetPosition {
            position: position.position,
            taken_at: position.taken_at,
        })
    }
}
