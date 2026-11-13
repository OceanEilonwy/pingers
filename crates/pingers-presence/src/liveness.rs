// SPDX-License-Identifier: AGPL-3.0-only
//! The poster's decision on a verified knock (design B.4.4 steps 4 to 6).

use pingers_geo::PresenceState;
use pingers_proto::{GroupId, RecordRandomness};

use crate::VerifiedKnock;

/// What a fresh fix taken for the liveness check showed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FreshFix {
    /// Inside the Spot's fence.
    InsideSpot,
    /// Outside it.
    OutsideSpot,
    /// No fresh fix could be taken in time.
    Unavailable,
}

/// pingers-gate's answer, as this crate needs it (it cannot depend on pingers-gate, which is in
/// the same layer).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Sharing {
    /// The gate says share.
    Allowed,
    /// The gate says suppress.
    Suppressed,
}

/// The poster's re-validated state at knock time. This is what makes 60-minute leases safe: a
/// stale lease produces a decline, not a false Meetup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LivenessEvidence {
    /// The Spot's presence state now.
    pub spot_state: PresenceState,
    /// The fresh fix.
    pub fresh_fix: FreshFix,
    /// The gate's answer now (DND, pause, exclusion zones, read fresh at the knock).
    pub sharing: Sharing,
}

/// The poster's own side of a knock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KnockSituation<'records> {
    /// The record the knock answered.
    pub own_record: &'records RecordRandomness,
    /// Records this device has knocked and not yet seen answered.
    pub knocked_records: &'records [RecordRandomness],
    /// The Meetup this device is in at the Spot, if any.
    pub current_meetup: Option<GroupId>,
}

/// How to admit an accepted knocker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    /// Create a new Meetup group and add the knocker (design B.5).
    CreateMeetup,
    /// Add the knocker to the Meetup this device is already in at the Spot.
    AddToMeetup(GroupId),
}

/// The decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub enum KnockDecision {
    /// Admit the knocker and post the Welcome to its reply mailbox.
    Accept(Admission),
    /// Send nothing. Indistinguishable from "could not be woken" (design B.4.6).
    Decline,
    /// Both devices knocked each other and the peer's record is smaller: the peer creates the
    /// group; wait for its Welcome (design B.4.4 step 6).
    WaitForPeer,
}

/// Decides on a verified knock.
///
/// - Decline unless the gate allows sharing, the Spot is `Inside` and a fresh fix is inside it.
/// - Tie-break: if the knocker names its own record and this device has knocked that record
///   too, the device whose record randomness is smaller creates the group; the other waits.
/// - Otherwise accept, into the current Meetup if there is one.
///
pub fn decide_knock(
    knock: &VerifiedKnock,
    evidence: &LivenessEvidence,
    situation: &KnockSituation<'_>,
) -> KnockDecision {
    let KnockSituation {
        own_record,
        knocked_records,
        current_meetup,
    } = *situation;
    let live = evidence.sharing == Sharing::Allowed
        && evidence.spot_state == PresenceState::Inside
        && evidence.fresh_fix == FreshFix::InsideSpot;
    if !live {
        return KnockDecision::Decline;
    }
    if let Some(their_record) = &knock.knock.own_randomness
        && knocked_records.contains(their_record)
        && their_record < own_record
    {
        return KnockDecision::WaitForPeer;
    }
    match current_meetup {
        Some(group) => KnockDecision::Accept(Admission::AddToMeetup(group)),
        None => KnockDecision::Accept(Admission::CreateMeetup),
    }
}

#[cfg(test)]
mod tests {
    use pingers_crypto::AccountPublicKey;
    use pingers_proto::{Epoch, KeyPackageBytes, Knock, KnockSignature, LeafIndex, MailboxId};

    use super::*;

    fn record(fill: u8) -> RecordRandomness {
        RecordRandomness::from_bytes([fill; 16])
    }

    fn knock_from(own: Option<RecordRandomness>) -> VerifiedKnock {
        VerifiedKnock {
            knocker: LeafIndex::new(1),
            knocker_key: AccountPublicKey::from_bytes([1; 32]),
            knock: Knock {
                matched_randomness: record(5),
                own_randomness: own,
                leaf_index: LeafIndex::new(1),
                party_epoch: Epoch::new(3),
                key_package: KeyPackageBytes::from_bytes(Vec::new()),
                reply_mailbox: MailboxId::from_bytes([0; 16]),
                meetup_subscription: None,
                signature: KnockSignature::from_bytes([0; 64]),
            },
        }
    }

    const LIVE: LivenessEvidence = LivenessEvidence {
        spot_state: PresenceState::Inside,
        fresh_fix: FreshFix::InsideSpot,
        sharing: Sharing::Allowed,
    };

    #[test]
    fn live_poster_without_meetup_creates_one() {
        let decision = decide_knock(
            &knock_from(None),
            &LIVE,
            &KnockSituation {
                own_record: &record(5),
                knocked_records: &[],
                current_meetup: None,
            },
        );
        assert_eq!(decision, KnockDecision::Accept(Admission::CreateMeetup));
    }

    #[test]
    fn live_poster_in_a_meetup_adds_the_knocker() {
        let group = GroupId::from_bytes([7; 32]);
        let decision = decide_knock(
            &knock_from(None),
            &LIVE,
            &KnockSituation {
                own_record: &record(5),
                knocked_records: &[],
                current_meetup: Some(group),
            },
        );
        assert_eq!(
            decision,
            KnockDecision::Accept(Admission::AddToMeetup(group))
        );
    }

    #[test]
    fn stale_fix_declines() {
        let evidence = LivenessEvidence {
            fresh_fix: FreshFix::OutsideSpot,
            ..LIVE
        };
        assert_eq!(
            decide_knock(
                &knock_from(None),
                &evidence,
                &KnockSituation {
                    own_record: &record(5),
                    knocked_records: &[],
                    current_meetup: None
                }
            ),
            KnockDecision::Decline
        );
    }

    #[test]
    fn suppressed_poster_declines() {
        let evidence = LivenessEvidence {
            sharing: Sharing::Suppressed,
            ..LIVE
        };
        assert_eq!(
            decide_knock(
                &knock_from(None),
                &evidence,
                &KnockSituation {
                    own_record: &record(5),
                    knocked_records: &[],
                    current_meetup: None
                }
            ),
            KnockDecision::Decline
        );
    }

    #[test]
    fn candidate_spot_declines() {
        let evidence = LivenessEvidence {
            spot_state: PresenceState::Candidate,
            ..LIVE
        };
        assert_eq!(
            decide_knock(
                &knock_from(None),
                &evidence,
                &KnockSituation {
                    own_record: &record(5),
                    knocked_records: &[],
                    current_meetup: None
                }
            ),
            KnockDecision::Decline
        );
    }

    #[test]
    fn mutual_knock_with_smaller_peer_record_waits() {
        let knock = knock_from(Some(record(2)));
        assert_eq!(
            decide_knock(
                &knock,
                &LIVE,
                &KnockSituation {
                    own_record: &record(5),
                    knocked_records: &[record(2)],
                    current_meetup: None
                }
            ),
            KnockDecision::WaitForPeer
        );
    }

    #[test]
    fn mutual_knock_with_larger_peer_record_creates() {
        let knock = knock_from(Some(record(9)));
        assert_eq!(
            decide_knock(
                &knock,
                &LIVE,
                &KnockSituation {
                    own_record: &record(5),
                    knocked_records: &[record(9)],
                    current_meetup: None
                }
            ),
            KnockDecision::Accept(Admission::CreateMeetup)
        );
    }

    #[test]
    fn smaller_peer_record_we_did_not_knock_is_accepted() {
        let knock = knock_from(Some(record(2)));
        assert_eq!(
            decide_knock(
                &knock,
                &LIVE,
                &KnockSituation {
                    own_record: &record(5),
                    knocked_records: &[],
                    current_meetup: None
                }
            ),
            KnockDecision::Accept(Admission::CreateMeetup)
        );
    }
}
