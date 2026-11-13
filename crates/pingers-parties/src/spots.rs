// SPDX-License-Identifier: AGPL-3.0-only
//! Spots: create, edit (circle or polygon), delete (design G.2).

use pingers_geo::{Circle, Coordinate, Fence, MINIMUM_SPOT_RADIUS, Meters, Polygon};
use pingers_mls::MlsContext;
use pingers_proto::{Lamport, PartyId, SpotId};
use pingers_store::{Session, Spot, SpotName, SpotStatus};
use rand_core::CryptoRng;

use crate::lifecycle::{identity, party_row, random, seal_operation};
use crate::ops::PartyOperation;
use crate::{Outbound, PartiesError, PartyLifecycle};

/// A Spot's shape as drawn in the editor, before validation.
#[derive(Clone, Debug, PartialEq)]
pub enum SpotShape {
    /// A circle; the radius must be at least `pingers_geo::MINIMUM_SPOT_RADIUS`.
    Circle {
        /// The centre.
        center: Coordinate,
        /// The radius.
        radius: Meters,
    },
    /// A polygon, validated by pingers-geo: 3 to 100 vertices, no self-intersection, within
    /// 100 km.
    Polygon(Vec<Coordinate>),
}

impl SpotShape {
    /// The validated fence.
    ///
    /// # Errors
    ///
    /// [`PartiesError::SpotTooSmall`] for a circle under the minimum radius;
    /// [`PartiesError::Geo`] for an invalid polygon or a zero radius.
    pub fn into_fence(self) -> Result<Fence, PartiesError> {
        match self {
            Self::Circle { center, radius } => {
                if radius < MINIMUM_SPOT_RADIUS {
                    return Err(PartiesError::SpotTooSmall);
                }
                Ok(Fence::Circle(Circle::new(center, radius)?))
            }
            Self::Polygon(vertices) => Ok(Fence::Polygon(Polygon::new(vertices)?)),
        }
    }
}

/// A Spot to create.
pub struct NewSpot {
    /// Its name.
    pub name: SpotName,
    /// Its shape.
    pub shape: SpotShape,
}

/// What to change in a Spot; `None` keeps the current value.
pub struct SpotEdit {
    /// The Spot.
    pub spot: SpotId,
    /// A new name.
    pub name: Option<SpotName>,
    /// A new shape.
    pub shape: Option<SpotShape>,
}

/// A Spot change made on this device, and the message that tells the other members.
pub struct SpotChange {
    /// The Spot.
    pub spot: SpotId,
    /// [`Outbound::GroupMessage`] carrying the operation.
    pub outbound: Outbound,
}

impl PartyLifecycle<'_> {
    /// Creates a Spot with a random id (design G.2).
    ///
    /// # Errors
    ///
    /// [`PartiesError::SpotTooSmall`] or [`PartiesError::Geo`] for an invalid shape;
    /// [`PartiesError::UnknownParty`]; [`PartiesError::Mls`] (`CommitPending` while a commit
    /// or a leaver's proposal waits); [`PartiesError::Store`].
    pub fn create_spot(
        &self,
        party: &PartyId,
        new: NewSpot,
        rng: &mut impl CryptoRng,
    ) -> Result<SpotChange, PartiesError> {
        let NewSpot { name, shape } = new;
        let fence = shape.into_fence()?;
        self.session.transaction(|session| {
            let created_by = identity(session)?.signing_key.public_key();
            let spot = Spot {
                id: SpotId::from_bytes(random(rng)),
                party: *party,
                name,
                fence,
                created_by,
                lamport: next_lamport(session, party)?,
                status: SpotStatus::Active,
            };
            publish(session, spot, rng)
        })
    }

    /// Edits a Spot's name or shape. Any member may edit any Spot; the latest edit wins.
    ///
    /// # Errors
    ///
    /// [`PartiesError::UnknownSpot`]; [`PartiesError::SpotDeleted`]; as
    /// [`Self::create_spot`] otherwise.
    pub fn edit_spot(
        &self,
        party: &PartyId,
        edit: SpotEdit,
        rng: &mut impl CryptoRng,
    ) -> Result<SpotChange, PartiesError> {
        let fence = edit.shape.map(SpotShape::into_fence).transpose()?;
        self.session.transaction(|session| {
            let mut current = active_spot(session, party, &edit.spot)?;
            if let Some(name) = edit.name {
                current.name = name;
            }
            if let Some(fence) = fence {
                current.fence = fence;
            }
            current.lamport = next_lamport(session, party)?;
            publish(session, current, rng)
        })
    }

    /// Deletes a Spot, leaving a tombstone so an older edit cannot bring it back.
    ///
    /// # Errors
    ///
    /// [`PartiesError::UnknownSpot`]; [`PartiesError::SpotDeleted`]; as
    /// [`Self::create_spot`] otherwise.
    pub fn delete_spot(
        &self,
        party: &PartyId,
        spot: &SpotId,
        rng: &mut impl CryptoRng,
    ) -> Result<SpotChange, PartiesError> {
        self.session.transaction(|session| {
            let mut current = active_spot(session, party, spot)?;
            current.status = SpotStatus::Deleted;
            current.lamport = next_lamport(session, party)?;
            publish(session, current, rng)
        })
    }

    /// The Party's Spots that are in use (tombstones left out), by id.
    ///
    /// # Errors
    ///
    /// [`PartiesError::Store`].
    pub fn spots(&self, party: &PartyId) -> Result<Vec<Spot>, PartiesError> {
        Ok(self
            .session
            .spots()
            .list(party)?
            .into_iter()
            .filter(|spot| spot.status == SpotStatus::Active)
            .collect())
    }
}

fn active_spot(session: &Session, party: &PartyId, spot: &SpotId) -> Result<Spot, PartiesError> {
    let current = session
        .spots()
        .get(spot)?
        .filter(|found| found.party == *party)
        .ok_or(PartiesError::UnknownSpot)?;
    if current.status == SpotStatus::Deleted {
        return Err(PartiesError::SpotDeleted);
    }
    Ok(current)
}

/// One more than the highest Lamport clock of any of the Party's Spots, tombstones included.
fn next_lamport(session: &Session, party: &PartyId) -> Result<Lamport, PartiesError> {
    session
        .spots()
        .list(party)?
        .iter()
        .map(|spot| spot.lamport)
        .max()
        .unwrap_or(Lamport::new(0))
        .checked_next()
        .ok_or(PartiesError::Internal)
}

/// Stores the Spot and seals the operation for the group, in the caller's transaction.
fn publish(
    session: &Session,
    spot: Spot,
    rng: &mut impl CryptoRng,
) -> Result<SpotChange, PartiesError> {
    let row = party_row(session, &spot.party)?;
    let identity = identity(session)?;
    let context = MlsContext::new(session, &identity.signing_key);
    let mut group = context.party_group(&row.group)?;
    session.spots().put(&spot)?;
    let id = spot.id;
    let outbound = seal_operation(&mut group, &PartyOperation::Spot(spot), rng)?;
    Ok(SpotChange { spot: id, outbound })
}
