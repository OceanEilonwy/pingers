// SPDX-License-Identifier: AGPL-3.0-only
//! Meetup frames: the plaintext of MLS application messages in a Meetup group (design B.6,
//! B.6.1). Encoded as CBOR and padded to 256 B, 1 KiB or 4 KiB ([`crate::SizeClass`]).

use core::fmt;

use minicbor::Decoder;
use pingers_time::{Bucket, UnixSeconds};

use crate::board::decode_bucket;
use crate::cbor::{self, CborBody, Writer, versioned_cbor};
use crate::{ChatMessageId, Lamport, LeafIndex, LocationGeneration, ProtoError, SegmentHash};

/// Latitude and longitude in units of 10⁻⁷ degrees, the wire form of a position.
///
/// `Debug` prints `E7Coordinate(redacted)` (style guide §9). Conversion to and from
/// pingers-geo's coordinate type happens in the crates that depend on both.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct E7Coordinate {
    latitude: i32,
    longitude: i32,
}

impl E7Coordinate {
    /// Checks the ranges: latitude within ±90°, longitude within ±180°.
    ///
    /// # Errors
    ///
    /// [`ProtoError::OutOfRange`] naming the field that is outside its range.
    pub const fn new(latitude_e7: i32, longitude_e7: i32) -> Result<Self, ProtoError> {
        if latitude_e7 < -900_000_000 || latitude_e7 > 900_000_000 {
            return Err(ProtoError::OutOfRange { field: "latitude" });
        }
        if longitude_e7 < -1_800_000_000 || longitude_e7 > 1_800_000_000 {
            return Err(ProtoError::OutOfRange { field: "longitude" });
        }
        Ok(Self {
            latitude: latitude_e7,
            longitude: longitude_e7,
        })
    }

    /// Latitude in 10⁻⁷ degrees.
    #[must_use]
    pub const fn latitude_e7(self) -> i32 {
        self.latitude
    }

    /// Longitude in 10⁻⁷ degrees.
    #[must_use]
    pub const fn longitude_e7(self) -> i32 {
        self.longitude
    }
}

impl fmt::Debug for E7Coordinate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("E7Coordinate(redacted)")
    }
}

/// Horizontal accuracy radius of a fix, in whole metres.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AccuracyMeters(u16);

impl AccuracyMeters {
    /// Wraps an accuracy.
    #[must_use]
    pub const fn new(meters: u16) -> Self {
        Self(meters)
    }

    /// The accuracy in metres.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// A compass heading in whole degrees, 0 to 359, clockwise from true north.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HeadingDegrees(u16);

impl HeadingDegrees {
    /// Checks the range.
    ///
    /// # Errors
    ///
    /// [`ProtoError::OutOfRange`] if `degrees` is 360 or more.
    pub const fn new(degrees: u16) -> Result<Self, ProtoError> {
        if degrees < 360 {
            Ok(Self(degrees))
        } else {
            Err(ProtoError::OutOfRange { field: "heading" })
        }
    }

    /// The heading in degrees.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// Ground speed in centimetres per second.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SpeedCentimetresPerSecond(u16);

impl SpeedCentimetresPerSecond {
    /// Wraps a speed.
    #[must_use]
    pub const fn new(centimetres_per_second: u16) -> Self {
        Self(centimetres_per_second)
    }

    /// The speed in centimetres per second.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// The inner plaintext of [`MeetupMessage::SealedLocation`], after decryption with the
/// sender's location key (design B.6). Never sent outside that inner encryption.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocationPlaintext {
    /// Where the sender is.
    pub position: E7Coordinate,
    /// How accurate the fix is.
    pub accuracy: AccuracyMeters,
    /// Which way the sender faces, if known.
    pub heading: Option<HeadingDegrees>,
    /// How fast the sender moves, if known.
    pub speed: Option<SpeedCentimetresPerSecond>,
    /// When the fix was taken.
    pub taken_at: UnixSeconds,
}

/// The sealed exact position: `AEAD(LK[generation], LocationPlaintext)` (design B.6.1).
#[derive(Clone, PartialEq, Eq)]
pub struct SealedLocationFrame {
    /// The location key generation it is sealed under.
    pub generation: LocationGeneration,
    /// The 12-byte AES-256-GCM nonce.
    pub nonce: [u8; 12],
    /// Ciphertext and tag.
    pub ciphertext: Vec<u8>,
}

impl fmt::Debug for SealedLocationFrame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SealedLocationFrame")
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

/// A location grant, HPKE-sealed to one member's grant key; names no recipient (design B.6.1).
#[derive(Clone, PartialEq, Eq)]
pub struct SealedGrant(Vec<u8>);

/// A location request, HPKE-sealed to the target's grant key; names no requester outside the
/// seal (design B.6.1).
#[derive(Clone, PartialEq, Eq)]
pub struct SealedAsk(Vec<u8>);

/// Implements byte access and a length-only `Debug` for sealed byte strings.
macro_rules! sealed_vec {
    ($($name:ident),*) => {
        $(
            impl $name {
                /// Wraps sealed bytes.
                #[must_use]
                pub const fn from_bytes(bytes: Vec<u8>) -> Self {
                    Self(bytes)
                }

                /// The sealed bytes.
                #[must_use]
                pub fn as_bytes(&self) -> &[u8] {
                    &self.0
                }
            }

            impl fmt::Debug for $name {
                fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                    write!(formatter, concat!(stringify!($name), "({} bytes)"), self.0.len())
                }
            }
        )*
    };
}

sealed_vec!(SealedGrant, SealedAsk);

/// The text of a chat message. `Debug` prints only its length.
#[derive(Clone, PartialEq, Eq)]
pub struct ChatBody(String);

impl ChatBody {
    /// The longest body in bytes, so a chat frame fits the 4 KiB class with room for the
    /// envelope.
    pub const MAX_BYTES: usize = 3_500;

    /// Checks the length.
    ///
    /// # Errors
    ///
    /// [`ProtoError::OutOfRange`] if the body is empty or longer than [`ChatBody::MAX_BYTES`].
    pub fn new(text: String) -> Result<Self, ProtoError> {
        if text.is_empty() || text.len() > Self::MAX_BYTES {
            Err(ProtoError::OutOfRange { field: "chat body" })
        } else {
            Ok(Self(text))
        }
    }

    /// The text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ChatBody {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ChatBody({} bytes)", self.0.len())
    }
}

/// A fixed meet point's optional label. `Debug` prints only its length.
#[derive(Clone, PartialEq, Eq)]
pub struct MeetPointLabel(String);

impl MeetPointLabel {
    /// The longest label in bytes.
    pub const MAX_BYTES: usize = 120;

    /// Checks the length.
    ///
    /// # Errors
    ///
    /// [`ProtoError::OutOfRange`] if the label is empty or longer than
    /// [`MeetPointLabel::MAX_BYTES`].
    pub fn new(text: String) -> Result<Self, ProtoError> {
        if text.is_empty() || text.len() > Self::MAX_BYTES {
            Err(ProtoError::OutOfRange {
                field: "meet point label",
            })
        } else {
            Ok(Self(text))
        }
    }

    /// The text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for MeetPointLabel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "MeetPointLabel({} bytes)", self.0.len())
    }
}

/// The Meetup's meet point; last writer wins by `(lamport, sender leaf)` (design B.6, B.6.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MeetPoint {
    /// A fixed place.
    Fixed {
        /// Where.
        position: E7Coordinate,
        /// What to call it.
        label: Option<MeetPointLabel>,
        /// Lamport clock of this write.
        lamport: Lamport,
    },
    /// "Meet at me": valid only when sent by `leaf` itself, and implicitly grants everyone
    /// that member's location while it stays the meet point.
    AtMember {
        /// The member whose live position is the meet point.
        leaf: LeafIndex,
        /// Lamport clock of this write.
        lamport: Lamport,
    },
    /// No meet point.
    Cleared {
        /// Lamport clock of this write.
        lamport: Lamport,
    },
}

/// One closed board segment a member saw before the Meetup, for the equivocation check
/// (design B.4.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeenSegment {
    /// The segment's start bucket.
    pub start: Bucket,
    /// The hash the server reported for it.
    pub hash: SegmentHash,
}

/// A Meetup frame: one MLS application message in the Meetup group (design B.6).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum MeetupMessage {
    /// Exact position under the sender's location key, or random bytes of the same length
    /// under a generation nobody holds. Ephemeral; sent at a fixed cadence.
    SealedLocation(SealedLocationFrame),
    /// A location key, sealed to one member.
    LocationGrant(SealedGrant),
    /// A request for a member's location key, sealed to that member.
    LocationAsk(SealedAsk),
    /// A chat message; threads are one level deep.
    Chat {
        /// Random id.
        id: ChatMessageId,
        /// The root message this replies to, if any.
        parent: Option<ChatMessageId>,
        /// The text.
        body: ChatBody,
        /// When it was written, by the sender's clock.
        written_at: UnixSeconds,
    },
    /// A new meet point.
    MeetPoint(MeetPoint),
    /// State for a late joiner, sent by the lowest online leaf after an Add.
    Snapshot {
        /// The current meet point, if any.
        meet_point: Option<MeetPoint>,
        /// The sender's Lamport clock.
        lamport: Lamport,
    },
    /// The board segment hashes a member saw; each member sends this as its first message.
    BoardSeen {
        /// The closed segments it saw.
        segments: Vec<SeenSegment>,
    },
    /// "Still here". Ephemeral.
    KeepAlive,
}

/// Variant numbers of [`MeetupMessage`] on the wire.
mod message_kind {
    pub(super) const SEALED_LOCATION: u8 = 0;
    pub(super) const LOCATION_GRANT: u8 = 1;
    pub(super) const LOCATION_ASK: u8 = 2;
    pub(super) const CHAT: u8 = 3;
    pub(super) const MEET_POINT: u8 = 4;
    pub(super) const SNAPSHOT: u8 = 5;
    pub(super) const BOARD_SEEN: u8 = 6;
    pub(super) const KEEP_ALIVE: u8 = 7;
}

/// Variant numbers of [`MeetPoint`] on the wire.
mod meet_point_kind {
    pub(super) const FIXED: u8 = 0;
    pub(super) const AT_MEMBER: u8 = 1;
    pub(super) const CLEARED: u8 = 2;
}

impl E7Coordinate {
    /// Writes latitude then longitude, as two integers in the enclosing array.
    fn encode_fields(self, writer: &mut Writer) {
        writer.signed(self.latitude);
        writer.signed(self.longitude);
    }

    /// Reads and checks latitude then longitude.
    fn decode_fields(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        let latitude = decoder.i32()?;
        let longitude = decoder.i32()?;
        Self::new(latitude, longitude)
    }
}

/// Layout: `[0, latitude, longitude, label or null, lamport]`, `[1, leaf, lamport]` or
/// `[2, lamport]`.
impl CborBody for MeetPoint {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        match self {
            Self::Fixed {
                position,
                label,
                lamport,
            } => {
                writer.array(5)?;
                writer.unsigned(meet_point_kind::FIXED);
                position.encode_fields(writer);
                writer.option(label.as_ref(), |writer, label| writer.text(label.as_str()))?;
                writer.unsigned(lamport.get());
            }
            Self::AtMember { leaf, lamport } => {
                writer.array(3)?;
                writer.unsigned(meet_point_kind::AT_MEMBER);
                writer.unsigned(leaf.get());
                writer.unsigned(lamport.get());
            }
            Self::Cleared { lamport } => {
                writer.array(2)?;
                writer.unsigned(meet_point_kind::CLEARED);
                writer.unsigned(lamport.get());
            }
        }
        Ok(())
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        let length = cbor::array(decoder)?;
        match decoder.u8()? {
            meet_point_kind::FIXED => {
                cbor::expect_length(length, 5)?;
                Ok(Self::Fixed {
                    position: E7Coordinate::decode_fields(decoder)?,
                    label: cbor::option(decoder, |decoder| {
                        MeetPointLabel::new(decoder.str()?.to_owned())
                    })?,
                    lamport: Lamport::new(decoder.u64()?),
                })
            }
            meet_point_kind::AT_MEMBER => {
                cbor::expect_length(length, 3)?;
                Ok(Self::AtMember {
                    leaf: LeafIndex::new(decoder.u32()?),
                    lamport: Lamport::new(decoder.u64()?),
                })
            }
            meet_point_kind::CLEARED => {
                cbor::expect_length(length, 2)?;
                Ok(Self::Cleared {
                    lamport: Lamport::new(decoder.u64()?),
                })
            }
            _ => Err(ProtoError::OutOfRange {
                field: "meet point kind",
            }),
        }
    }
}

/// Layout: `[start bucket, hash]`.
impl CborBody for SeenSegment {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(2)?;
        writer.unsigned(self.start.index());
        writer.bytes(self.hash.as_bytes())
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 2)?;
        Ok(Self {
            start: decode_bucket(decoder)?,
            hash: SegmentHash::from_bytes(cbor::fixed_bytes(decoder)?),
        })
    }
}

/// Layout: `[kind, fields...]` with kinds 0 to 7 in declaration order:
///
/// - `[0, generation, nonce (12 bytes), ciphertext]`
/// - `[1, sealed grant]`, `[2, sealed ask]`
/// - `[3, id, parent or null, body text, written at]`
/// - `[4, meet point]`
/// - `[5, meet point or null, lamport]`
/// - `[6, [[start bucket, hash], ...]]`
/// - `[7]`
impl CborBody for MeetupMessage {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        match self {
            Self::SealedLocation(frame) => {
                writer.array(4)?;
                writer.unsigned(message_kind::SEALED_LOCATION);
                writer.unsigned(frame.generation.get());
                writer.bytes(&frame.nonce)?;
                writer.bytes(&frame.ciphertext)?;
            }
            Self::LocationGrant(grant) => {
                writer.array(2)?;
                writer.unsigned(message_kind::LOCATION_GRANT);
                writer.bytes(grant.as_bytes())?;
            }
            Self::LocationAsk(ask) => {
                writer.array(2)?;
                writer.unsigned(message_kind::LOCATION_ASK);
                writer.bytes(ask.as_bytes())?;
            }
            Self::Chat {
                id,
                parent,
                body,
                written_at,
            } => {
                writer.array(5)?;
                writer.unsigned(message_kind::CHAT);
                writer.bytes(id.as_bytes())?;
                writer.option(parent.as_ref(), |writer, parent| {
                    writer.bytes(parent.as_bytes())
                })?;
                writer.text(body.as_str())?;
                writer.unsigned(written_at.get());
            }
            Self::MeetPoint(meet_point) => {
                writer.array(2)?;
                writer.unsigned(message_kind::MEET_POINT);
                meet_point.encode_body(writer)?;
            }
            Self::Snapshot {
                meet_point,
                lamport,
            } => {
                writer.array(3)?;
                writer.unsigned(message_kind::SNAPSHOT);
                writer.option(meet_point.as_ref(), |writer, meet_point| {
                    meet_point.encode_body(writer)
                })?;
                writer.unsigned(lamport.get());
            }
            Self::BoardSeen { segments } => {
                writer.array(2)?;
                writer.unsigned(message_kind::BOARD_SEEN);
                writer.list(segments)?;
            }
            Self::KeepAlive => {
                writer.array(1)?;
                writer.unsigned(message_kind::KEEP_ALIVE);
            }
        }
        Ok(())
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        let length = cbor::array(decoder)?;
        let kind = decoder.u8()?;
        let expected_length = match kind {
            message_kind::KEEP_ALIVE => 1,
            message_kind::LOCATION_GRANT
            | message_kind::LOCATION_ASK
            | message_kind::MEET_POINT
            | message_kind::BOARD_SEEN => 2,
            message_kind::SNAPSHOT => 3,
            message_kind::SEALED_LOCATION => 4,
            message_kind::CHAT => 5,
            _ => {
                return Err(ProtoError::OutOfRange {
                    field: "meetup message kind",
                });
            }
        };
        cbor::expect_length(length, expected_length)?;
        Ok(match kind {
            message_kind::SEALED_LOCATION => Self::SealedLocation(SealedLocationFrame {
                generation: LocationGeneration::new(decoder.u32()?),
                nonce: cbor::fixed_bytes(decoder)?,
                ciphertext: decoder.bytes()?.to_vec(),
            }),
            message_kind::LOCATION_GRANT => {
                Self::LocationGrant(SealedGrant::from_bytes(decoder.bytes()?.to_vec()))
            }
            message_kind::LOCATION_ASK => {
                Self::LocationAsk(SealedAsk::from_bytes(decoder.bytes()?.to_vec()))
            }
            message_kind::CHAT => Self::Chat {
                id: ChatMessageId::from_bytes(cbor::fixed_bytes(decoder)?),
                parent: cbor::option(decoder, |decoder| {
                    cbor::fixed_bytes(decoder).map(ChatMessageId::from_bytes)
                })?,
                body: ChatBody::new(decoder.str()?.to_owned())?,
                written_at: UnixSeconds::new(decoder.u64()?),
            },
            message_kind::MEET_POINT => Self::MeetPoint(MeetPoint::decode_body(decoder)?),
            message_kind::SNAPSHOT => Self::Snapshot {
                meet_point: cbor::option(decoder, MeetPoint::decode_body)?,
                lamport: Lamport::new(decoder.u64()?),
            },
            message_kind::BOARD_SEEN => Self::BoardSeen {
                segments: cbor::list(decoder)?,
            },
            _ => Self::KeepAlive,
        })
    }
}

/// Layout: `[latitude, longitude, accuracy, heading or null, speed or null, taken at]`.
impl CborBody for LocationPlaintext {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(6)?;
        self.position.encode_fields(writer);
        writer.unsigned(self.accuracy.get());
        writer.option(self.heading.as_ref(), |writer, heading| {
            writer.unsigned(heading.get());
            Ok(())
        })?;
        writer.option(self.speed.as_ref(), |writer, speed| {
            writer.unsigned(speed.get());
            Ok(())
        })?;
        writer.unsigned(self.taken_at.get());
        Ok(())
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 6)?;
        Ok(Self {
            position: E7Coordinate::decode_fields(decoder)?,
            accuracy: AccuracyMeters::new(decoder.u16()?),
            heading: cbor::option(decoder, |decoder| HeadingDegrees::new(decoder.u16()?))?,
            speed: cbor::option(decoder, |decoder| {
                Ok(SpeedCentimetresPerSecond::new(decoder.u16()?))
            })?,
            taken_at: UnixSeconds::new(decoder.u64()?),
        })
    }
}

versioned_cbor!(MeetupMessage, LocationPlaintext);

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    #[test]
    fn latitude_beyond_the_pole_is_rejected() {
        assert!(E7Coordinate::new(900_000_001, 0).is_err());
        assert!(E7Coordinate::new(900_000_000, 1_800_000_000).is_ok());
    }

    #[test]
    fn coordinates_are_redacted_in_debug_output() {
        let coordinate = E7Coordinate::new(515_000_000, -1_000_000);
        assert_eq!(format!("{coordinate:?}"), "Ok(E7Coordinate(redacted))");
    }

    #[test]
    fn coordinate_keeps_latitude_and_longitude_apart() -> Result<(), ProtoError> {
        let coordinate = E7Coordinate::new(-338_688_000, 1_512_093_000)?;
        assert_eq!(coordinate.latitude_e7(), -338_688_000);
        assert_eq!(coordinate.longitude_e7(), 1_512_093_000);
        Ok(())
    }

    #[test]
    fn empty_chat_is_rejected() {
        assert!(ChatBody::new(String::new()).is_err());
    }
}
