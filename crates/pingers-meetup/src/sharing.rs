// SPDX-License-Identifier: AGPL-3.0-only
//! This device's side of exact-location consent: its location key and generation, who holds
//! it, and the grants that keep holders up to date (design B.6.1).
//!
//! Who should hold the key is derived, never stored directly: the visibility setting, the
//! approvals given, the people revoked, and whether the meet point is "at me". After every
//! change [`Sharing::reconcile`] compares that with who was actually granted the current
//! generation. Anyone who lost access forces a rotation (fresh key, next generation) and new
//! grants to everyone who keeps it; anyone new, or whose end changed, gets a grant.
//!
//! Traffic stays uniform where it would otherwise reveal the setting or the number of
//! holders: a rotation always sends exactly one grant-sized frame per other member, real or
//! dummy, and so does a membership change for each newcomer, whether the setting is
//! `Everyone` (real grant) or not (dummy).
//!
//! Grants, real or dummy, are stored in the group log (no wake), so a member whose live
//! channel is down when one is sent still receives it when it reads the log after
//! reconnecting. Real and dummy grants share the same delivery, so the log shows the same
//! count of stored grant-sized messages either way.

use std::collections::{BTreeMap, BTreeSet};

use pingers_crypto::{
    AccountPublicKey, GrantContext, GrantKeyPair, LocationAad, LocationGrant, LocationKey,
    LocationToSeal, dummy_location, seal_location, seal_location_grant,
};
use pingers_proto::{
    Delivery, Epoch, GroupId, LeafIndex, LocationGeneration, LocationPlaintext, MeetupMessage,
    SealedLocationFrame, Urgency,
};
use pingers_time::UnixSeconds;
use rand_core::CryptoRng;

use crate::session::{Member, OutgoingFrame};
use crate::{AlwaysAllow, ExactLocation, GrantScope, LocationVisibility, MeetupError, SHORT_GRANT};

/// When a grant lapses. Ordered so that the later end is the greater.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum GrantEnd {
    /// At this time.
    At(UnixSeconds),
    /// When the Meetup ends or either member leaves.
    MeetupEnd,
}

/// How "until the Meetup ends" is written in a grant's `expires_at`: the largest timestamp,
/// which no clock reaches. Only at this wire boundary; [`GrantEnd::MeetupEnd`] elsewhere.
const MEETUP_END_ON_WIRE: UnixSeconds = UnixSeconds::new(u64::MAX);

impl GrantEnd {
    pub(crate) fn from_scope(scope: GrantScope, now: UnixSeconds) -> Result<Self, MeetupError> {
        match scope {
            GrantScope::ThisMeetup => Ok(Self::MeetupEnd),
            GrantScope::FifteenMinutes => now
                .checked_add(SHORT_GRANT)
                .map(Self::At)
                .ok_or(MeetupError::Overflow),
        }
    }

    pub(crate) const fn on_wire(self) -> UnixSeconds {
        match self {
            Self::At(time) => time,
            Self::MeetupEnd => MEETUP_END_ON_WIRE,
        }
    }

    pub(crate) fn from_wire(expires_at: UnixSeconds) -> Self {
        if expires_at == MEETUP_END_ON_WIRE {
            Self::MeetupEnd
        } else {
            Self::At(expires_at)
        }
    }

    pub(crate) const fn access(self) -> ExactLocation {
        match self {
            Self::At(time) => ExactLocation::Shared {
                expires: Some(time),
            },
            Self::MeetupEnd => ExactLocation::Shared { expires: None },
        }
    }

    pub(crate) fn has_lapsed(self, now: UnixSeconds) -> bool {
        match self {
            Self::At(time) => time <= now,
            Self::MeetupEnd => false,
        }
    }

    pub(crate) const fn deadline(self) -> Option<UnixSeconds> {
        match self {
            Self::At(time) => Some(time),
            Self::MeetupEnd => None,
        }
    }
}

/// Whether the meet point is this member's own live position, which implicitly grants
/// everyone while it lasts (design B.6.1 "Meet point").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MeetAtMe {
    /// "Meet at me" is the current meet point.
    Active,
    /// It is not.
    Inactive,
}

/// Which members a reconcile covers with one grant-sized frame each, whether or not their
/// access changed.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Padding<'leaves> {
    /// Only members whose access changed get a frame.
    ChangesOnly,
    /// Each of these members gets exactly one frame: a real grant if their access changed,
    /// otherwise a dummy. Members outside the set whose access changed get a real grant too.
    Each(&'leaves BTreeSet<LeafIndex>),
}

/// Whether a reconcile must rotate even if nobody lost access.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rotation {
    /// Rotate only if someone lost access.
    IfRevoked,
    /// Always rotate (a revoke action, or a member left).
    Force,
}

/// How a reconcile pads and whether it must rotate.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Plan<'leaves> {
    pub(crate) padding: Padding<'leaves>,
    pub(crate) rotation: Rotation,
}

/// Who a real grant goes to, and until when.
#[derive(Clone, Copy, Debug)]
struct Target<'member> {
    member: &'member Member,
    end: GrantEnd,
}

/// What a reconcile needs to know about the group.
pub(crate) struct Roster<'roster> {
    pub(crate) group: &'roster GroupId,
    pub(crate) own_leaf: LeafIndex,
    /// The other members.
    pub(crate) others: &'roster BTreeMap<LeafIndex, Member>,
    pub(crate) meet_at_me: MeetAtMe,
}

/// This device's location key and who holds it.
pub(crate) struct Sharing {
    visibility: LocationVisibility,
    always_allow: AlwaysAllow,
    generation: LocationGeneration,
    key: LocationKey,
    /// Approvals given (explicitly, or automatically for the allow-list).
    approved: BTreeMap<LeafIndex, GrantEnd>,
    /// Members revoked in this Meetup: neither `Everyone` nor "meet at me" grants them until
    /// they are approved again.
    excluded: BTreeSet<LeafIndex>,
    /// Who was sent the current generation, and with which end.
    granted: BTreeMap<LeafIndex, GrantEnd>,
}

impl Sharing {
    pub(crate) fn new(
        visibility: LocationVisibility,
        always_allow: AlwaysAllow,
        rng: &mut impl CryptoRng,
    ) -> Self {
        Self {
            visibility,
            always_allow,
            generation: LocationGeneration::new(0),
            key: LocationKey::generate(rng),
            approved: BTreeMap::new(),
            excluded: BTreeSet::new(),
            granted: BTreeMap::new(),
        }
    }

    pub(crate) const fn visibility(&self) -> LocationVisibility {
        self.visibility
    }

    #[cfg(test)]
    pub(crate) const fn generation(&self) -> LocationGeneration {
        self.generation
    }

    pub(crate) fn set_visibility(&mut self, visibility: LocationVisibility) {
        self.visibility = visibility;
        if visibility == LocationVisibility::Nobody {
            self.approved.clear();
        }
    }

    pub(crate) fn is_always_allowed(&self, member: &AccountPublicKey) -> bool {
        self.always_allow.contains(member)
    }

    pub(crate) fn allow_always(&mut self, member: AccountPublicKey) {
        self.always_allow.insert(member);
    }

    pub(crate) fn is_excluded(&self, leaf: LeafIndex) -> bool {
        self.excluded.contains(&leaf)
    }

    /// Records an approval; it lifts an earlier revoke. A second approval keeps the later end.
    pub(crate) fn approve(&mut self, leaf: LeafIndex, end: GrantEnd) {
        self.excluded.remove(&leaf);
        let entry = self.approved.entry(leaf).or_insert(end);
        *entry = (*entry).max(end);
    }

    pub(crate) fn revoke(&mut self, leaf: LeafIndex) {
        self.approved.remove(&leaf);
        self.excluded.insert(leaf);
    }

    /// Forgets a member who left.
    pub(crate) fn forget(&mut self, leaf: LeafIndex) {
        self.approved.remove(&leaf);
        self.excluded.remove(&leaf);
    }

    /// Drops approvals that have lapsed by `now`; reports whether any did.
    pub(crate) fn expire(&mut self, now: UnixSeconds) -> bool {
        let before = self.approved.len();
        self.approved.retain(|_leaf, end| !end.has_lapsed(now));
        self.approved.len() != before
    }

    /// The earliest approval end, for the session's deadline.
    pub(crate) fn next_expiry(&self) -> Option<UnixSeconds> {
        self.approved
            .values()
            .filter_map(|end| end.deadline())
            .min()
    }

    /// Whether a member was sent the current generation.
    pub(crate) fn holds(&self, leaf: LeafIndex) -> bool {
        self.granted.contains_key(&leaf)
    }

    /// Makes the next reconcile send `leaf` its grant again, as if it were new.
    pub(crate) fn resend(&mut self, leaf: LeafIndex) {
        self.granted.remove(&leaf);
    }

    /// Whether anyone holds the current generation.
    pub(crate) fn has_holders(&self) -> bool {
        !self.granted.is_empty()
    }

    fn desired(&self, roster: &Roster<'_>) -> BTreeMap<LeafIndex, GrantEnd> {
        if self.visibility == LocationVisibility::Nobody {
            return BTreeMap::new();
        }
        let implicit = (self.visibility == LocationVisibility::Everyone
            || roster.meet_at_me == MeetAtMe::Active)
            .then_some(GrantEnd::MeetupEnd);
        roster
            .others
            .keys()
            .filter(|leaf| !self.excluded.contains(leaf))
            .filter_map(|leaf| {
                implicit
                    .max(self.approved.get(leaf).copied())
                    .map(|end| (*leaf, end))
            })
            .collect()
    }

    /// Brings the holders of the location key in line with who should hold it, and returns
    /// the grant frames to send (see the module documentation).
    ///
    /// # Errors
    ///
    /// [`MeetupError::Overflow`] if the generation is exhausted; [`MeetupError::Crypto`] if a
    /// grant cannot be sealed.
    pub(crate) fn reconcile(
        &mut self,
        roster: &Roster<'_>,
        plan: Plan<'_>,
        rng: &mut impl CryptoRng,
    ) -> Result<Vec<OutgoingFrame>, MeetupError> {
        let Plan { padding, rotation } = plan;
        let desired = self.desired(roster);
        let lost_access = self.granted.keys().any(|leaf| !desired.contains_key(leaf));
        let all_others: BTreeSet<LeafIndex> = roster.others.keys().copied().collect();
        let (padded, previous) = if lost_access || rotation == Rotation::Force {
            self.generation = self
                .generation
                .checked_next()
                .ok_or(MeetupError::Overflow)?;
            self.key = LocationKey::generate(rng);
            (all_others, BTreeMap::new())
        } else {
            let padded = match padding {
                Padding::ChangesOnly => BTreeSet::new(),
                Padding::Each(leaves) => leaves.clone(),
            };
            (padded, core::mem::take(&mut self.granted))
        };
        let mut frames = Vec::new();
        for (leaf, member) in roster.others {
            let wanted = desired.get(leaf).copied();
            let frame = match wanted {
                Some(end) if previous.get(leaf) != Some(&end) => {
                    self.grant(roster, Target { member, end }, rng)?
                }
                Some(_) | None if padded.contains(leaf) => self.dummy_grant(roster, member, rng)?,
                Some(_) | None => continue,
            };
            frames.push(frame);
        }
        self.granted = desired;
        Ok(frames)
    }

    fn grant(
        &self,
        roster: &Roster<'_>,
        target: Target<'_>,
        rng: &mut impl CryptoRng,
    ) -> Result<OutgoingFrame, MeetupError> {
        let Target { member, end } = target;
        let grant = LocationGrant {
            context: GrantContext {
                group: roster.group,
                sender: roster.own_leaf,
            },
            generation: self.generation,
            key: &self.key,
            expires_at: end.on_wire(),
        };
        let sealed = seal_location_grant(&member.grant_key, &grant, rng)?;
        Ok(consent(MeetupMessage::LocationGrant(sealed)))
    }

    /// A grant-sized frame nobody can open: a grant of a throwaway key sealed to a throwaway
    /// grant key of the same suite as `member`'s, so it has exactly the structure and length
    /// of a real grant to that member (an X-Wing grant is far larger than an X25519 one).
    fn dummy_grant(
        &self,
        roster: &Roster<'_>,
        member: &Member,
        rng: &mut impl CryptoRng,
    ) -> Result<OutgoingFrame, MeetupError> {
        let throwaway = GrantKeyPair::generate_like(&member.grant_key, rng)?;
        let key = LocationKey::generate(rng);
        let grant = LocationGrant {
            context: GrantContext {
                group: roster.group,
                sender: roster.own_leaf,
            },
            generation: self.generation,
            key: &key,
            expires_at: MEETUP_END_ON_WIRE,
        };
        let sealed = seal_location_grant(&throwaway.public, &grant, rng)?;
        Ok(consent(MeetupMessage::LocationGrant(sealed)))
    }

    /// This slot's location frame: the position sealed under the current key if anyone holds
    /// it and a fix is known, otherwise random bytes of the same length under the same
    /// generation. Either way it looks the same to anyone without the key.
    ///
    /// # Errors
    ///
    /// [`MeetupError::Crypto`] if sealing fails.
    pub(crate) fn location_frame(
        &self,
        aad: FrameContext<'_>,
        position: Option<&LocationPlaintext>,
        rng: &mut impl CryptoRng,
    ) -> Result<SealedLocationFrame, MeetupError> {
        match position {
            Some(location) if self.has_holders() => {
                let frame = LocationToSeal {
                    aad: LocationAad {
                        group: aad.group,
                        epoch: aad.epoch,
                        sender: aad.sender,
                        generation: self.generation,
                    },
                    location,
                };
                Ok(seal_location(&self.key, &frame, rng)?)
            }
            Some(_) | None => Ok(dummy_location(self.generation, rng)),
        }
    }
}

/// Group, epoch and sender of an outgoing location frame.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FrameContext<'group> {
    pub(crate) group: &'group GroupId,
    pub(crate) epoch: Epoch,
    pub(crate) sender: LeafIndex,
}

/// A frame for live channels only: never stored, never pushed.
pub(crate) const fn ephemeral(message: MeetupMessage) -> OutgoingFrame {
    OutgoingFrame {
        message,
        delivery: Delivery::Ephemeral,
        wake: None,
    }
}

/// A frame kept in the group log until the Meetup ends.
pub(crate) const fn stored(message: MeetupMessage, wake: Option<Urgency>) -> OutgoingFrame {
    OutgoingFrame {
        message,
        delivery: Delivery::Stored,
        wake,
    }
}

/// A grant or ask, real or dummy: stored, so a member whose live channel is down reads it
/// from the log on reconnecting, and never pushed, so it wakes nobody (a wake would show
/// that a real grant or ask was aimed at someone absent).
pub(crate) const fn consent(message: MeetupMessage) -> OutgoingFrame {
    stored(message, None)
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_crypto::open_location;
    use pingers_proto::{AccuracyMeters, E7Coordinate};
    use pingers_testkit::seeded_rng;

    use super::*;

    #[test]
    fn with_no_holders_even_a_known_position_goes_out_as_a_dummy() -> Result<(), MeetupError> {
        let mut rng = seeded_rng(1);
        let sharing = Sharing::new(
            LocationVisibility::AskEachTime,
            AlwaysAllow::default(),
            &mut rng,
        );
        let group = GroupId::from_bytes([1; 32]);
        let context = FrameContext {
            group: &group,
            epoch: Epoch::new(1),
            sender: LeafIndex::new(0),
        };
        let position = LocationPlaintext {
            position: E7Coordinate::new(1, 1)?,
            accuracy: AccuracyMeters::new(5),
            heading: None,
            speed: None,
            taken_at: UnixSeconds::new(1),
        };
        let frame = sharing.location_frame(context, Some(&position), &mut rng)?;
        let aad = LocationAad {
            group: &group,
            epoch: Epoch::new(1),
            sender: LeafIndex::new(0),
            generation: sharing.generation(),
        };
        // Not even the device's own key opens it: there is no position inside.
        assert!(open_location(&sharing.key, &aad, &frame).is_err());
        Ok(())
    }

    #[test]
    fn meetup_end_round_trips_through_the_wire_value() {
        assert_eq!(
            GrantEnd::from_wire(GrantEnd::MeetupEnd.on_wire()),
            GrantEnd::MeetupEnd
        );
        let at = GrantEnd::At(UnixSeconds::new(1_000));
        assert_eq!(GrantEnd::from_wire(at.on_wire()), at);
    }

    #[test]
    fn meetup_end_outlasts_any_time() {
        assert!(GrantEnd::MeetupEnd > GrantEnd::At(UnixSeconds::new(u64::MAX - 1)));
        assert!(GrantEnd::At(UnixSeconds::new(2)) > GrantEnd::At(UnixSeconds::new(1)));
    }

    #[test]
    fn a_timed_grant_lapses_at_its_end_not_before() {
        let end = GrantEnd::At(UnixSeconds::new(100));
        assert!(!end.has_lapsed(UnixSeconds::new(99)));
        assert!(end.has_lapsed(UnixSeconds::new(100)));
        assert!(!GrantEnd::MeetupEnd.has_lapsed(UnixSeconds::new(u64::MAX)));
    }
}
