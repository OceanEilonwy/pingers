// SPDX-License-Identifier: AGPL-3.0-only
//! Party state (design B.2): the operations members send as Party group application messages
//! (roster profiles, Spot create, edit and delete) and the state snapshot a committer uploads
//! for new members.
//!
//! Geometry travels as IEEE 754 doubles, exactly as the sender's pingers-geo values, so every
//! member validates and stores bit-identical fences; this crate checks only that they are
//! finite. Polygon validity and the minimum radius are pingers-geo's and pingers-parties' to
//! check.

use core::fmt;

use minicbor::Decoder;

use crate::cbor::{self, CborBody, Writer, versioned_cbor};
use crate::{DisplayName, Lamport, PartyId, ProtoError, SpotId, SubscriptionId};

/// Defines a 1 to 200 byte UTF-8 name with a redacted `Debug`.
macro_rules! bounded_name {
    ($(#[$meta:meta])* $name:ident, $field:literal) => {
        $(#[$meta])*
        #[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            /// The longest name in bytes (as pingers-store's names).
            pub const MAX_BYTES: usize = 200;

            /// Checks the length.
            ///
            /// # Errors
            ///
            /// [`ProtoError::OutOfRange`] if it is empty or longer than `MAX_BYTES`.
            pub fn new(name: String) -> Result<Self, ProtoError> {
                if name.is_empty() || name.len() > Self::MAX_BYTES {
                    Err(ProtoError::OutOfRange { field: $field })
                } else {
                    Ok(Self(name))
                }
            }

            /// The name.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(concat!(stringify!($name), "(redacted)"))
            }
        }
    };
}

bounded_name!(
    /// A Party's name in its snapshot.
    PartyTitle,
    "party name"
);
bounded_name!(
    /// A Spot's name.
    SpotTitle,
    "spot name"
);

/// A member's Ed25519 account key (their leaf signature key, design B.1), as bytes. pingers-
/// crypto's `AccountPublicKey` is the domain type; this crate has no crypto. `Debug` does not
/// print it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MemberKey([u8; 32]);

impl MemberKey {
    /// Wraps key bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The key bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for MemberKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("MemberKey(redacted)")
    }
}

/// A WGS84 point of a Spot's fence in degrees, as finite doubles. `Debug` does not print it.
#[derive(Clone, Copy, PartialEq)]
pub struct FencePoint {
    latitude: f64,
    longitude: f64,
}

impl FencePoint {
    /// Checks that both are finite (pingers-geo checks the ranges).
    ///
    /// # Errors
    ///
    /// [`ProtoError::OutOfRange`] for a NaN or infinite value.
    pub fn new(latitude: f64, longitude: f64) -> Result<Self, ProtoError> {
        Ok(Self {
            latitude: finite(latitude, "latitude")?,
            longitude: finite(longitude, "longitude")?,
        })
    }

    /// Latitude in degrees.
    #[must_use]
    pub const fn latitude(self) -> f64 {
        self.latitude
    }

    /// Longitude in degrees.
    #[must_use]
    pub const fn longitude(self) -> f64 {
        self.longitude
    }

    fn encode_fields(self, writer: &mut Writer) {
        writer.double(self.latitude);
        writer.double(self.longitude);
    }

    fn decode_fields(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        Self::new(decoder.f64()?, decoder.f64()?)
    }
}

impl fmt::Debug for FencePoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("FencePoint(redacted)")
    }
}

fn finite(value: f64, field: &'static str) -> Result<f64, ProtoError> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(ProtoError::OutOfRange { field })
    }
}

/// A Spot's geofence.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum SpotFence {
    /// A circle around `center`.
    Circle {
        /// The centre.
        center: FencePoint,
        /// The radius in metres (finite; pingers-parties enforces the minimum).
        radius_meters: f64,
    },
    /// A polygon through these vertices, in order.
    Polygon(Vec<FencePoint>),
}

mod fence_kind {
    pub(super) const CIRCLE: u8 = 0;
    pub(super) const POLYGON: u8 = 1;
}

/// Layout: `[0, latitude, longitude, radius]` or `[1, [[latitude, longitude], ...]]`, every
/// number an eight-byte float.
impl CborBody for SpotFence {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        match self {
            Self::Circle {
                center,
                radius_meters,
            } => {
                writer.array(4)?;
                writer.unsigned(fence_kind::CIRCLE);
                center.encode_fields(writer);
                writer.double(*radius_meters);
            }
            Self::Polygon(vertices) => {
                writer.array(2)?;
                writer.unsigned(fence_kind::POLYGON);
                writer.array(vertices.len())?;
                for vertex in vertices {
                    writer.array(2)?;
                    vertex.encode_fields(writer);
                }
            }
        }
        Ok(())
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        let length = cbor::array(decoder)?;
        match decoder.u8()? {
            fence_kind::CIRCLE => {
                cbor::expect_length(length, 4)?;
                Ok(Self::Circle {
                    center: FencePoint::decode_fields(decoder)?,
                    radius_meters: finite(decoder.f64()?, "radius")?,
                })
            }
            fence_kind::POLYGON => {
                cbor::expect_length(length, 2)?;
                let count = cbor::array(decoder)?;
                // No preallocation: the count comes from the wire.
                let mut vertices = Vec::new();
                for _ in 0..count {
                    cbor::array_of(decoder, 2)?;
                    vertices.push(FencePoint::decode_fields(decoder)?);
                }
                Ok(Self::Polygon(vertices))
            }
            _ => Err(ProtoError::OutOfRange {
                field: "fence kind",
            }),
        }
    }
}

/// Whether a Spot exists or was deleted (a tombstone, kept so a late edit cannot revive it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpotLife {
    /// The Spot exists.
    Active,
    /// The Spot was deleted.
    Deleted,
}

/// A Spot's full state as one member last wrote it; last writer wins by Lamport clock.
#[derive(Clone, Debug, PartialEq)]
pub struct SpotState {
    /// The Spot.
    pub id: SpotId,
    /// Its name.
    pub name: SpotTitle,
    /// Its fence.
    pub fence: SpotFence,
    /// The member who created it (never changes).
    pub created_by: MemberKey,
    /// The Lamport clock of this write.
    pub lamport: Lamport,
    /// Active or deleted.
    pub life: SpotLife,
}

/// Layout: `[spot id (16), name, fence, created by (32), lamport, deleted (0 or 1)]`.
impl CborBody for SpotState {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(6)?;
        writer.bytes(self.id.as_bytes())?;
        writer.text(self.name.as_str())?;
        self.fence.encode_body(writer)?;
        writer.bytes(self.created_by.as_bytes())?;
        writer.unsigned(self.lamport.get());
        writer.unsigned(match self.life {
            SpotLife::Active => 0_u8,
            SpotLife::Deleted => 1,
        });
        Ok(())
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 6)?;
        Ok(Self {
            id: SpotId::from_bytes(cbor::fixed_bytes(decoder)?),
            name: SpotTitle::new(decoder.str()?.to_owned())?,
            fence: SpotFence::decode_body(decoder)?,
            created_by: MemberKey::from_bytes(cbor::fixed_bytes(decoder)?),
            lamport: Lamport::new(decoder.u64()?),
            life: match decoder.u8()? {
                0 => SpotLife::Active,
                1 => SpotLife::Deleted,
                _ => return Err(ProtoError::OutOfRange { field: "spot life" }),
            },
        })
    }
}

/// A member's roster profile as they publish it: their display name and, with wake-up pushes
/// on, the id of their sealed-push subscription in the Party group, so whoever removes them can
/// list it in the remove commit's `subscriptions_removed`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileUpdate {
    /// The name the roster shows.
    pub name: DisplayName,
    /// The sender's subscription in the Party group, if any.
    pub subscription: Option<SubscriptionId>,
}

/// Layout: `[name, subscription id (16) or null]`.
impl CborBody for ProfileUpdate {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(2)?;
        writer.text(self.name.as_str())?;
        writer.option(self.subscription.as_ref(), |writer, subscription| {
            writer.bytes(subscription.as_bytes())
        })
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 2)?;
        Ok(Self {
            name: DisplayName::new(decoder.str()?.to_owned())?,
            subscription: cbor::option(decoder, |decoder| {
                Ok(SubscriptionId::from_bytes(cbor::fixed_bytes(decoder)?))
            })?,
        })
    }
}

/// A Party state operation (design B.2), the plaintext of a Party group application message
/// before padding. The sender is the MLS sender; nothing here names it.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum PartyOperation {
    /// The sender's roster profile, sent on joining and whenever it changes.
    Profile(ProfileUpdate),
    /// A Spot created, edited or deleted.
    Spot(SpotState),
}

mod operation_kind {
    pub(super) const PROFILE: u8 = 0;
    pub(super) const SPOT: u8 = 1;
}

/// Layout: `[0, profile]` or `[1, spot]`.
impl CborBody for PartyOperation {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(2)?;
        match self {
            Self::Profile(profile) => {
                writer.unsigned(operation_kind::PROFILE);
                profile.encode_body(writer)
            }
            Self::Spot(spot) => {
                writer.unsigned(operation_kind::SPOT);
                spot.encode_body(writer)
            }
        }
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 2)?;
        match decoder.u8()? {
            operation_kind::PROFILE => Ok(Self::Profile(ProfileUpdate::decode_body(decoder)?)),
            operation_kind::SPOT => Ok(Self::Spot(SpotState::decode_body(decoder)?)),
            _ => Err(ProtoError::OutOfRange {
                field: "party operation kind",
            }),
        }
    }
}

/// One member's profile in a snapshot, by key (a new member matches keys to leaves).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberProfile {
    /// The member's account key.
    pub key: MemberKey,
    /// Their latest profile.
    pub profile: ProfileUpdate,
}

/// Layout: `[key (32), profile]`.
impl CborBody for MemberProfile {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(2)?;
        writer.bytes(self.key.as_bytes())?;
        self.profile.encode_body(writer)
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 2)?;
        Ok(Self {
            key: MemberKey::from_bytes(cbor::fixed_bytes(decoder)?),
            profile: ProfileUpdate::decode_body(decoder)?,
        })
    }
}

/// The Party state a committer seals for the next epoch (design B.2): what a new member cannot
/// read from history.
#[derive(Clone, Debug, PartialEq)]
pub struct PartySnapshot {
    /// The Party.
    pub party: PartyId,
    /// Its name.
    pub name: PartyTitle,
    /// Every member's profile the committer knows.
    pub members: Vec<MemberProfile>,
    /// Every Spot, tombstones included.
    pub spots: Vec<SpotState>,
}

/// Layout: `[party id (16), name, [member profile, ...], [spot, ...]]`.
impl CborBody for PartySnapshot {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(4)?;
        writer.bytes(self.party.as_bytes())?;
        writer.text(self.name.as_str())?;
        writer.list(&self.members)?;
        writer.list(&self.spots)
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 4)?;
        Ok(Self {
            party: PartyId::from_bytes(cbor::fixed_bytes(decoder)?),
            name: PartyTitle::new(decoder.str()?.to_owned())?,
            members: cbor::list(decoder)?,
            spots: cbor::list(decoder)?,
        })
    }
}

versioned_cbor!(PartyOperation, PartySnapshot);
