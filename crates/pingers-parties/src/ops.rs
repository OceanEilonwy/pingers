// SPDX-License-Identifier: AGPL-3.0-only
//! Party state operations (MLS application messages) and the state snapshot (design B.2), as
//! this crate's domain values. Their wire forms are pingers-proto's `PartyOperation` and
//! `PartySnapshot`; this module converts both ways, checking geometry with pingers-geo and
//! names with pingers-store's types on the way in.

use pingers_crypto::AccountPublicKey;
use pingers_geo::{Circle, Coordinate, Fence, Meters, Polygon};
use pingers_proto::{
    DisplayName, FencePoint, MemberKey, MemberProfile, PartyId, PartySnapshot, PartyTitle,
    ProfileUpdate, SpotFence, SpotLife, SpotState, SpotTitle, SubscriptionId, WireDecode as _,
    WireEncode as _,
};
use pingers_store::{MemberName, PartyName, Spot, SpotName, SpotStatus};

use crate::PartiesError;

/// One Party state operation.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PartyOperation {
    /// The sender's roster profile.
    Profile {
        name: MemberName,
        /// The sender's sealed-push subscription in the Party group, so whoever removes the
        /// sender later can drop it from the server (`CommitRequest::subscriptions_removed`).
        subscription: Option<SubscriptionId>,
    },
    /// A Spot created, edited or deleted (a tombstone), last writer wins by Lamport clock.
    Spot(Spot),
}

impl PartyOperation {
    pub(crate) fn encode(&self) -> Result<Vec<u8>, PartiesError> {
        let wire = match self {
            Self::Profile { name, subscription } => {
                pingers_proto::PartyOperation::Profile(profile_to_wire(name, *subscription)?)
            }
            Self::Spot(spot) => pingers_proto::PartyOperation::Spot(spot_to_wire(spot)?),
        };
        Ok(wire.encode()?)
    }

    /// Decodes an operation of `party` (Spots are filed under the Party they arrived in, not
    /// a Party named inside the message).
    pub(crate) fn decode(bytes: &[u8], party: PartyId) -> Result<Self, PartiesError> {
        match pingers_proto::PartyOperation::decode(bytes)? {
            pingers_proto::PartyOperation::Profile(profile) => Ok(Self::Profile {
                name: member_name(&profile.name)?,
                subscription: profile.subscription,
            }),
            pingers_proto::PartyOperation::Spot(spot) => {
                Ok(Self::Spot(spot_from_wire(spot, party)?))
            }
            _ => Err(PartiesError::Malformed {
                what: "party operation",
            }),
        }
    }
}

fn profile_to_wire(
    name: &MemberName,
    subscription: Option<SubscriptionId>,
) -> Result<ProfileUpdate, PartiesError> {
    Ok(ProfileUpdate {
        name: DisplayName::new(name.as_str().to_owned())?,
        subscription,
    })
}

fn member_name(name: &DisplayName) -> Result<MemberName, PartiesError> {
    Ok(MemberName::new(name.as_str().to_owned())?)
}

fn spot_to_wire(spot: &Spot) -> Result<SpotState, PartiesError> {
    Ok(SpotState {
        id: spot.id,
        name: SpotTitle::new(spot.name.as_str().to_owned())?,
        fence: fence_to_wire(&spot.fence)?,
        created_by: MemberKey::from_bytes(*spot.created_by.as_bytes()),
        lamport: spot.lamport,
        life: match spot.status {
            SpotStatus::Active => SpotLife::Active,
            SpotStatus::Deleted => SpotLife::Deleted,
        },
    })
}

fn spot_from_wire(spot: SpotState, party: PartyId) -> Result<Spot, PartiesError> {
    Ok(Spot {
        id: spot.id,
        party,
        name: SpotName::new(spot.name.as_str().to_owned())?,
        fence: fence_from_wire(spot.fence)?,
        created_by: AccountPublicKey::from_bytes(*spot.created_by.as_bytes()),
        lamport: spot.lamport,
        status: match spot.life {
            SpotLife::Active => SpotStatus::Active,
            SpotLife::Deleted => SpotStatus::Deleted,
        },
    })
}

fn point_to_wire(coordinate: Coordinate) -> Result<FencePoint, PartiesError> {
    Ok(FencePoint::new(
        coordinate.latitude(),
        coordinate.longitude(),
    )?)
}

fn point_from_wire(point: FencePoint) -> Result<Coordinate, PartiesError> {
    Ok(Coordinate::new(point.latitude(), point.longitude())?)
}

fn fence_to_wire(fence: &Fence) -> Result<SpotFence, PartiesError> {
    Ok(match fence {
        Fence::Circle(circle) => SpotFence::Circle {
            center: point_to_wire(circle.center())?,
            radius_meters: circle.radius().get(),
        },
        Fence::Polygon(polygon) => SpotFence::Polygon(
            polygon
                .vertices()
                .iter()
                .map(|vertex| point_to_wire(*vertex))
                .collect::<Result<_, _>>()?,
        ),
    })
}

/// A received fence, checked by pingers-geo exactly as the sender's was.
fn fence_from_wire(fence: SpotFence) -> Result<Fence, PartiesError> {
    match fence {
        SpotFence::Circle {
            center,
            radius_meters,
        } => Ok(Fence::Circle(Circle::new(
            point_from_wire(center)?,
            Meters::new(radius_meters)?,
        )?)),
        SpotFence::Polygon(vertices) => Ok(Fence::Polygon(Polygon::new(
            vertices
                .into_iter()
                .map(point_from_wire)
                .collect::<Result<_, _>>()?,
        )?)),
        _ => Err(PartiesError::Malformed { what: "spot fence" }),
    }
}

/// Whether `incoming` replaces `stored`: the higher Lamport clock wins; on a tie the
/// bytewise larger wire encoding wins, so every member settles on the same version whatever
/// order they saw the two in.
pub(crate) fn spot_wins(incoming: &Spot, stored: &Spot) -> Result<bool, PartiesError> {
    if incoming.lamport != stored.lamport {
        return Ok(incoming.lamport > stored.lamport);
    }
    let encode = |spot: &Spot| PartyOperation::Spot(spot.clone()).encode();
    Ok(encode(incoming)? > encode(stored)?)
}

/// A member's profile, by key (leaves are not known before a Welcome is processed).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Profile {
    pub(crate) key: AccountPublicKey,
    pub(crate) name: MemberName,
    /// Their subscription in the Party group, so a later member can drop it on removal.
    pub(crate) subscription: Option<SubscriptionId>,
}

/// What a new member needs to know: the Party's id and name, its members' profiles, its Spots.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Snapshot {
    pub(crate) party: PartyId,
    pub(crate) name: PartyName,
    pub(crate) profiles: Vec<Profile>,
    pub(crate) spots: Vec<Spot>,
}

impl Snapshot {
    pub(crate) fn encode(&self) -> Result<Vec<u8>, PartiesError> {
        let wire = PartySnapshot {
            party: self.party,
            name: PartyTitle::new(self.name.as_str().to_owned())?,
            members: self
                .profiles
                .iter()
                .map(|profile| {
                    Ok(MemberProfile {
                        key: MemberKey::from_bytes(*profile.key.as_bytes()),
                        profile: profile_to_wire(&profile.name, profile.subscription)?,
                    })
                })
                .collect::<Result<_, PartiesError>>()?,
            spots: self
                .spots
                .iter()
                .map(spot_to_wire)
                .collect::<Result<_, _>>()?,
        };
        Ok(wire.encode()?)
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, PartiesError> {
        let wire = PartySnapshot::decode(bytes)?;
        let party = wire.party;
        Ok(Self {
            party,
            name: PartyName::new(wire.name.as_str().to_owned())?,
            profiles: wire
                .members
                .into_iter()
                .map(|member| {
                    Ok(Profile {
                        key: AccountPublicKey::from_bytes(*member.key.as_bytes()),
                        name: member_name(&member.profile.name)?,
                        subscription: member.profile.subscription,
                    })
                })
                .collect::<Result<_, PartiesError>>()?,
            spots: wire
                .spots
                .into_iter()
                .map(|spot| spot_from_wire(spot, party))
                .collect::<Result<_, _>>()?,
        })
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_proto::{Lamport, SpotId};

    use super::*;
    use crate::test_support::{TestError, circle_fence};

    fn spot(lamport: u64, name: &str) -> Result<Spot, TestError> {
        Ok(Spot {
            id: SpotId::from_bytes([1; 16]),
            party: PartyId::from_bytes([2; 16]),
            name: SpotName::new(name.into())?,
            fence: circle_fence(150.0)?,
            created_by: AccountPublicKey::from_bytes([3; 32]),
            lamport: Lamport::new(lamport),
            status: SpotStatus::Active,
        })
    }

    fn polygon() -> Result<Fence, TestError> {
        Ok(Fence::Polygon(Polygon::new(vec![
            Coordinate::new(51.5, -0.1)?,
            Coordinate::new(51.501, -0.1)?,
            Coordinate::new(51.501, -0.098)?,
        ])?))
    }

    #[test]
    fn operations_round_trip() -> Result<(), TestError> {
        let party = PartyId::from_bytes([2; 16]);
        let mut deleted = spot(4, "Pub")?;
        deleted.status = SpotStatus::Deleted;
        let mut shaped = spot(5, "Park")?;
        shaped.fence = polygon()?;
        for operation in [
            PartyOperation::Profile {
                name: MemberName::new("Bob".into())?,
                subscription: Some(SubscriptionId::from_bytes([7; 16])),
            },
            PartyOperation::Profile {
                name: MemberName::new("Bob".into())?,
                subscription: None,
            },
            PartyOperation::Spot(spot(3, "Pub")?),
            PartyOperation::Spot(deleted),
            PartyOperation::Spot(shaped),
        ] {
            assert_eq!(
                PartyOperation::decode(&operation.encode()?, party)?,
                operation
            );
        }
        Ok(())
    }

    #[test]
    fn a_received_spot_is_filed_under_the_party_it_arrived_in() -> Result<(), TestError> {
        let bytes = PartyOperation::Spot(spot(1, "Pub")?).encode()?;
        let other = PartyId::from_bytes([9; 16]);
        let PartyOperation::Spot(received) = PartyOperation::decode(&bytes, other)? else {
            return Err(TestError::Unexpected("expected a Spot"));
        };
        assert_eq!(received.party, other);
        Ok(())
    }

    fn wire_spot(fence: SpotFence) -> Result<Vec<u8>, TestError> {
        Ok(pingers_proto::PartyOperation::Spot(SpotState {
            id: SpotId::from_bytes([1; 16]),
            name: SpotTitle::new("Pub".into())?,
            fence,
            created_by: MemberKey::from_bytes([3; 32]),
            lamport: Lamport::new(1),
            life: SpotLife::Active,
        })
        .encode()?)
    }

    #[test]
    fn geometry_pingers_geo_refuses_is_refused_on_receipt() -> Result<(), TestError> {
        let party = PartyId::from_bytes([2; 16]);
        let two_points = wire_spot(SpotFence::Polygon(vec![
            FencePoint::new(1.0, 2.0)?,
            FencePoint::new(1.5, 2.0)?,
        ]))?;
        assert!(matches!(
            PartyOperation::decode(&two_points, party),
            Err(PartiesError::Geo(_))
        ));
        let off_the_globe = wire_spot(SpotFence::Circle {
            center: FencePoint::new(91.0, 0.0)?,
            radius_meters: 150.0,
        })?;
        assert!(matches!(
            PartyOperation::decode(&off_the_globe, party),
            Err(PartiesError::Geo(_))
        ));
        let negative = wire_spot(SpotFence::Circle {
            center: FencePoint::new(1.0, 0.0)?,
            radius_meters: -1.0,
        })?;
        assert!(matches!(
            PartyOperation::decode(&negative, party),
            Err(PartiesError::Geo(_))
        ));
        assert!(matches!(
            PartyOperation::decode(&[0x82, 0x01], party),
            Err(PartiesError::Proto(_))
        ));
        Ok(())
    }

    #[test]
    fn later_lamport_wins_and_ties_break_the_same_way_both_ways() -> Result<(), TestError> {
        assert!(spot_wins(&spot(2, "A")?, &spot(1, "B")?)?);
        assert!(!spot_wins(&spot(1, "B")?, &spot(2, "A")?)?);
        let (first, second) = (spot(5, "A")?, spot(5, "B")?);
        // The larger encoding ("B" sorts after "A") wins the tie.
        assert!(spot_wins(&second, &first)?);
        assert!(!spot_wins(&first, &second)?);
        assert!(!spot_wins(&first, &first)?);
        Ok(())
    }

    #[test]
    fn snapshots_round_trip_with_subscriptions() -> Result<(), TestError> {
        let snapshot = Snapshot {
            party: PartyId::from_bytes([2; 16]),
            name: PartyName::new("Friends".into())?,
            profiles: vec![
                Profile {
                    key: AccountPublicKey::from_bytes([3; 32]),
                    name: MemberName::new("Alice".into())?,
                    subscription: Some(SubscriptionId::from_bytes([4; 16])),
                },
                Profile {
                    key: AccountPublicKey::from_bytes([5; 32]),
                    name: MemberName::new("Bob".into())?,
                    subscription: None,
                },
            ],
            spots: vec![spot(1, "Pub")?],
        };
        assert_eq!(Snapshot::decode(&snapshot.encode()?)?, snapshot);
        let mut bytes = snapshot.encode()?;
        if let Some(version) = bytes.get_mut(1) {
            *version = 2;
        }
        assert!(Snapshot::decode(&bytes).is_err());
        Ok(())
    }
}
