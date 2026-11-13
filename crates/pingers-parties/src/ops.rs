// SPDX-License-Identifier: AGPL-3.0-only
//! Party state operations (MLS application messages) and the state snapshot (design B.2).
//!
//! Layouts (version byte 1, see `codec`):
//! - operation: `1 || tag || body`; tag 0 profile `name || has_sub (1) || sub (16)?`, tag 1
//!   Spot `spot`.
//! - spot: `id (16) || name || fence || created_by (32) || be64(lamport) || deleted (1)`.
//! - snapshot: `1 || party_id (16) || party name || count || (key (32) || name)* || count ||
//!   spot*`.

use pingers_crypto::AccountPublicKey;
use pingers_proto::{Lamport, PartyId, SpotId, SubscriptionId};
use pingers_store::{MemberName, PartyName, Spot, SpotName, SpotStatus};

use crate::PartiesError;
use crate::codec::{Reader, Writer};

const VERSION: u8 = 1;
const PROFILE: u8 = 0;
const SPOT: u8 = 1;

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
        let mut writer = Writer::new();
        writer.byte(VERSION);
        match self {
            Self::Profile { name, subscription } => {
                writer.byte(PROFILE);
                writer.variable(name.as_str().as_bytes())?;
                match subscription {
                    None => writer.byte(0),
                    Some(subscription) => {
                        writer.byte(1);
                        writer.fixed(subscription.as_bytes());
                    }
                }
            }
            Self::Spot(spot) => {
                writer.byte(SPOT);
                write_spot(&mut writer, spot)?;
            }
        }
        Ok(writer.finish())
    }

    /// Decodes an operation of `party` (Spots are filed under the Party they arrived in, not
    /// a Party named inside the message).
    pub(crate) fn decode(bytes: &[u8], party: PartyId) -> Result<Self, PartiesError> {
        let mut reader = Reader::new(bytes, "party operation");
        if reader.byte()? != VERSION {
            return Err(reader.malformed());
        }
        let operation = match reader.byte()? {
            PROFILE => {
                let name = MemberName::new(reader.text()?)?;
                let subscription = match reader.byte()? {
                    0 => None,
                    1 => Some(SubscriptionId::from_bytes(reader.fixed()?)),
                    _ => return Err(reader.malformed()),
                };
                Self::Profile { name, subscription }
            }
            SPOT => Self::Spot(read_spot(&mut reader, party)?),
            _ => return Err(reader.malformed()),
        };
        reader.end()?;
        Ok(operation)
    }
}

pub(crate) fn write_spot(writer: &mut Writer, spot: &Spot) -> Result<(), PartiesError> {
    writer.fixed(spot.id.as_bytes());
    writer.variable(spot.name.as_str().as_bytes())?;
    writer.fence(&spot.fence)?;
    writer.key(&spot.created_by);
    writer.u64(spot.lamport.get());
    writer.byte(match spot.status {
        SpotStatus::Active => 0,
        SpotStatus::Deleted => 1,
    });
    Ok(())
}

fn read_spot(reader: &mut Reader<'_>, party: PartyId) -> Result<Spot, PartiesError> {
    Ok(Spot {
        id: SpotId::from_bytes(reader.fixed()?),
        party,
        name: SpotName::new(reader.text()?)?,
        fence: reader.fence()?,
        created_by: reader.key()?,
        lamport: Lamport::new(reader.u64()?),
        status: match reader.byte()? {
            0 => SpotStatus::Active,
            1 => SpotStatus::Deleted,
            _ => return Err(reader.malformed()),
        },
    })
}

/// Whether `incoming` replaces `stored`: the higher Lamport clock wins; on a tie the
/// bytewise larger encoding wins, so every member settles on the same version whatever order
/// they saw the two in.
pub(crate) fn spot_wins(incoming: &Spot, stored: &Spot) -> Result<bool, PartiesError> {
    if incoming.lamport != stored.lamport {
        return Ok(incoming.lamport > stored.lamport);
    }
    let encode = |spot: &Spot| {
        let mut writer = Writer::new();
        write_spot(&mut writer, spot).map(|()| writer.finish())
    };
    Ok(encode(incoming)? > encode(stored)?)
}

/// A member's display name, by key (leaves are not known before a Welcome is processed).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Profile {
    pub(crate) key: AccountPublicKey,
    pub(crate) name: MemberName,
}

/// What a new member needs to know: the Party's id and name, its members' names, its Spots.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Snapshot {
    pub(crate) party: PartyId,
    pub(crate) name: PartyName,
    pub(crate) profiles: Vec<Profile>,
    pub(crate) spots: Vec<Spot>,
}

impl Snapshot {
    pub(crate) fn encode(&self) -> Result<Vec<u8>, PartiesError> {
        let mut writer = Writer::new();
        writer.byte(VERSION);
        writer.fixed(self.party.as_bytes());
        writer.variable(self.name.as_str().as_bytes())?;
        writer.count(self.profiles.len())?;
        for profile in &self.profiles {
            writer.key(&profile.key);
            writer.variable(profile.name.as_str().as_bytes())?;
        }
        writer.count(self.spots.len())?;
        for spot in &self.spots {
            write_spot(&mut writer, spot)?;
        }
        Ok(writer.finish())
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, PartiesError> {
        let mut reader = Reader::new(bytes, "state snapshot");
        if reader.byte()? != VERSION {
            return Err(reader.malformed());
        }
        let party = PartyId::from_bytes(reader.fixed()?);
        let name = PartyName::new(reader.text()?)?;
        let mut profiles = Vec::new();
        for _ in 0..reader.count()? {
            profiles.push(Profile {
                key: reader.key()?,
                name: MemberName::new(reader.text()?)?,
            });
        }
        let mut spots = Vec::new();
        for _ in 0..reader.count()? {
            spots.push(read_spot(&mut reader, party)?);
        }
        reader.end()?;
        Ok(Self {
            party,
            name,
            profiles,
            spots,
        })
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

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

    #[test]
    fn operations_round_trip() -> Result<(), TestError> {
        let party = PartyId::from_bytes([2; 16]);
        let mut deleted = spot(4, "Pub")?;
        deleted.status = SpotStatus::Deleted;
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
        ] {
            assert_eq!(
                PartyOperation::decode(&operation.encode()?, party)?,
                operation
            );
        }
        Ok(())
    }

    #[test]
    fn unknown_versions_tags_and_flags_are_refused() -> Result<(), TestError> {
        let party = PartyId::from_bytes([2; 16]);
        let profile = PartyOperation::Profile {
            name: MemberName::new("B".into())?,
            subscription: None,
        }
        .encode()?;
        // version, tag, subscription flag
        for index in [0, 1, profile.len().saturating_sub(1)] {
            let mut changed = profile.clone();
            if let Some(byte) = changed.get_mut(index) {
                *byte = 7;
            }
            assert!(PartyOperation::decode(&changed, party).is_err());
        }
        let spot = PartyOperation::Spot(spot(1, "Pub")?).encode()?;
        let mut bad_status = spot.clone();
        if let Some(byte) = bad_status.last_mut() {
            *byte = 2;
        }
        assert!(PartyOperation::decode(&bad_status, party).is_err());
        let mut longer = spot;
        longer.push(0);
        assert!(PartyOperation::decode(&longer, party).is_err());
        Ok(())
    }

    #[test]
    fn later_lamport_wins_and_ties_break_the_same_way_both_ways() -> Result<(), TestError> {
        assert!(spot_wins(&spot(2, "A")?, &spot(1, "B")?)?);
        assert!(!spot_wins(&spot(1, "B")?, &spot(2, "A")?)?);
        let (first, second) = (spot(5, "A")?, spot(5, "B")?);
        assert_ne!(spot_wins(&second, &first)?, spot_wins(&first, &second)?);
        assert!(!spot_wins(&first, &first)?);
        Ok(())
    }

    #[test]
    fn snapshots_round_trip() -> Result<(), TestError> {
        let snapshot = Snapshot {
            party: PartyId::from_bytes([2; 16]),
            name: PartyName::new("Friends".into())?,
            profiles: vec![Profile {
                key: AccountPublicKey::from_bytes([3; 32]),
                name: MemberName::new("Alice".into())?,
            }],
            spots: vec![spot(1, "Pub")?],
        };
        assert_eq!(Snapshot::decode(&snapshot.encode()?)?, snapshot);
        let mut bytes = snapshot.encode()?;
        if let Some(version) = bytes.first_mut() {
            *version = 2;
        }
        assert!(Snapshot::decode(&bytes).is_err());
        Ok(())
    }
}
