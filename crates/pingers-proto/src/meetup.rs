// SPDX-License-Identifier: AGPL-3.0-only
//! Meetup frames: the plaintext of MLS application messages in a Meetup group (design B.6,
//! B.6.1). Encoded as CBOR and padded to 256 B, 1 KiB or 4 KiB ([`crate::SizeClass`]).

use core::fmt;

use pingers_time::{Bucket, UnixSeconds};

use crate::stub::not_implemented;
use crate::{
    ChatMessageId, Lamport, LeafIndex, LocationGeneration, ProtoError, SegmentHash, WireDecode,
    WireEncode,
};

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

impl WireEncode for MeetupMessage {
    fn encode(&self) -> Result<Vec<u8>, ProtoError> {
        not_implemented(self)
    }
}

impl WireDecode for MeetupMessage {
    fn decode(bytes: &[u8]) -> Result<Self, ProtoError> {
        not_implemented(bytes)
    }
}

impl WireEncode for LocationPlaintext {
    fn encode(&self) -> Result<Vec<u8>, ProtoError> {
        not_implemented(self)
    }
}

impl WireDecode for LocationPlaintext {
    fn decode(bytes: &[u8]) -> Result<Self, ProtoError> {
        not_implemented(bytes)
    }
}

#[cfg(test)]
mod tests {
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
    fn empty_chat_is_rejected() {
        assert!(ChatBody::new(String::new()).is_err());
    }
}
