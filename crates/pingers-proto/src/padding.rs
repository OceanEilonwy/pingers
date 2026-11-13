// SPDX-License-Identifier: AGPL-3.0-only
//! Padding to fixed size classes, so sizes say nothing about content (design B.4.5, B.6).

use crate::ProtoError;

/// Bytes taken by the big-endian length prefix in front of a padded payload.
const LENGTH_PREFIX: usize = 2;

/// One of the fixed sizes that padded messages are sent at.
///
/// Meetup frames use 256 B, 1 KiB and 4 KiB ([`SizeClass::MEETUP_FRAME`]); mailbox posts use
/// 1, 4, 8 and 32 KiB ([`SizeClass::MAILBOX_POST`], design D).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum SizeClass {
    /// 256 bytes: location, keep-alive, grant and ask frames.
    Bytes256,
    /// 1 KiB: short chat; a knock under the classical suite.
    Bytes1024,
    /// 4 KiB: long chat; a knock under X-Wing; a classical Welcome.
    Bytes4096,
    /// 8 KiB: an X-Wing Welcome.
    Bytes8192,
    /// 32 KiB: the largest mailbox post.
    Bytes32768,
}

impl SizeClass {
    /// The classes a meetup frame may be padded to, smallest first.
    pub const MEETUP_FRAME: [Self; 3] = [Self::Bytes256, Self::Bytes1024, Self::Bytes4096];

    /// The classes a mailbox post may be padded to, smallest first.
    pub const MAILBOX_POST: [Self; 4] = [
        Self::Bytes1024,
        Self::Bytes4096,
        Self::Bytes8192,
        Self::Bytes32768,
    ];

    /// Every class, smallest first.
    const ALL: [Self; 5] = [
        Self::Bytes256,
        Self::Bytes1024,
        Self::Bytes4096,
        Self::Bytes8192,
        Self::Bytes32768,
    ];

    /// The padded size in bytes.
    #[must_use]
    pub const fn size(self) -> usize {
        match self {
            Self::Bytes256 => 256,
            Self::Bytes1024 => 1_024,
            Self::Bytes4096 => 4_096,
            Self::Bytes8192 => 8_192,
            Self::Bytes32768 => 32_768,
        }
    }

    /// The largest payload that fits this class.
    #[must_use]
    pub const fn capacity(self) -> usize {
        self.size().saturating_sub(LENGTH_PREFIX)
    }

    /// The class whose size is exactly `size`, if any.
    #[must_use]
    pub fn from_size(size: usize) -> Option<Self> {
        Self::ALL.into_iter().find(|class| class.size() == size)
    }
}

/// A payload padded to a size class: `be16(payload length) || payload || zero bytes`.
///
/// Only [`Padded::pad`], [`Padded::pad_smallest`] and [`Padded::from_wire`] make one, so a
/// value of this type always has exactly a class's size. Functions that send padded traffic
/// take `Padded`, which makes sending an unpadded payload a type error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Padded {
    class: SizeClass,
    bytes: Vec<u8>,
}

impl Padded {
    /// Pads `payload` to `class`.
    ///
    /// # Errors
    ///
    /// [`ProtoError::PayloadTooLarge`] if the payload exceeds [`SizeClass::capacity`].
    pub fn pad(payload: &[u8], class: SizeClass) -> Result<Self, ProtoError> {
        let too_large = ProtoError::PayloadTooLarge {
            length: payload.len(),
            class,
        };
        if payload.len() > class.capacity() {
            return Err(too_large);
        }
        let prefix = u16::try_from(payload.len()).map_err(|_overflow| too_large)?;
        let mut bytes = Vec::with_capacity(class.size());
        bytes.extend_from_slice(&prefix.to_be_bytes());
        bytes.extend_from_slice(payload);
        bytes.resize(class.size(), 0);
        Ok(Self { class, bytes })
    }

    /// Pads `payload` to the smallest of `classes` it fits.
    ///
    /// # Errors
    ///
    /// [`ProtoError::PayloadTooLarge`] (naming the largest class tried) if it fits none, or
    /// if `classes` is empty.
    pub fn pad_smallest(payload: &[u8], classes: &[SizeClass]) -> Result<Self, ProtoError> {
        let mut sorted = classes.to_vec();
        sorted.sort_unstable();
        let fitting = sorted
            .iter()
            .copied()
            .find(|class| payload.len() <= class.capacity());
        match (fitting, sorted.last()) {
            (Some(class), _) => Self::pad(payload, class),
            (None, Some(largest)) => Err(ProtoError::PayloadTooLarge {
                length: payload.len(),
                class: *largest,
            }),
            (None, None) => Err(ProtoError::PayloadTooLarge {
                length: payload.len(),
                class: SizeClass::Bytes256,
            }),
        }
    }

    /// Accepts bytes received from the network, checking only that their length is a class.
    /// Call [`Padded::payload`] to check the padding itself.
    ///
    /// # Errors
    ///
    /// [`ProtoError::NotASizeClass`] if the length matches no class.
    pub fn from_wire(bytes: Vec<u8>) -> Result<Self, ProtoError> {
        let class = SizeClass::from_size(bytes.len()).ok_or(ProtoError::NotASizeClass {
            length: bytes.len(),
        })?;
        Ok(Self { class, bytes })
    }

    /// The size class.
    #[must_use]
    pub const fn class(&self) -> SizeClass {
        self.class
    }

    /// The padded bytes, exactly [`SizeClass::size`] long.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The padded bytes, by value.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// The payload, after checking that the length prefix fits and the padding is all zero.
    ///
    /// # Errors
    ///
    /// [`ProtoError::BadPadding`] if the prefix claims more bytes than the class holds or a
    /// padding byte is not zero.
    pub fn payload(&self) -> Result<&[u8], ProtoError> {
        let (prefix, rest) = self
            .bytes
            .split_first_chunk::<LENGTH_PREFIX>()
            .ok_or(ProtoError::BadPadding)?;
        let length = usize::from(u16::from_be_bytes(*prefix));
        let (payload, padding) = rest
            .split_at_checked(length)
            .ok_or(ProtoError::BadPadding)?;
        if padding.iter().any(|byte| *byte != 0) {
            return Err(ProtoError::BadPadding);
        }
        Ok(payload)
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use proptest::prelude::*;

    use super::*;

    #[test]
    fn largest_payload_fills_the_class() -> Result<(), ProtoError> {
        let payload = vec![7; SizeClass::Bytes256.capacity()];
        let padded = Padded::pad(&payload, SizeClass::Bytes256)?;
        assert_eq!(padded.as_bytes().len(), 256);
        assert_eq!(padded.payload()?, payload.as_slice());
        Ok(())
    }

    #[test]
    fn one_byte_over_capacity_is_rejected() {
        let payload = vec![7; SizeClass::Bytes256.capacity().saturating_add(1)];
        assert!(matches!(
            Padded::pad(&payload, SizeClass::Bytes256),
            Err(ProtoError::PayloadTooLarge {
                class: SizeClass::Bytes256,
                ..
            })
        ));
    }

    #[test]
    fn smallest_fitting_class_is_chosen() -> Result<(), ProtoError> {
        let payload = vec![1; 300];
        let padded = Padded::pad_smallest(&payload, &SizeClass::MEETUP_FRAME)?;
        assert_eq!(padded.class(), SizeClass::Bytes1024);
        Ok(())
    }

    #[test]
    fn received_length_must_be_a_class() {
        assert!(matches!(
            Padded::from_wire(vec![0; 300]),
            Err(ProtoError::NotASizeClass { length: 300 })
        ));
    }

    #[test]
    fn nonzero_padding_is_rejected() -> Result<(), ProtoError> {
        let mut bytes = Padded::pad(b"hi", SizeClass::Bytes256)?.into_bytes();
        if let Some(last) = bytes.last_mut() {
            *last = 1;
        }
        assert!(matches!(
            Padded::from_wire(bytes)?.payload(),
            Err(ProtoError::BadPadding)
        ));
        Ok(())
    }

    #[test]
    fn length_prefix_beyond_the_class_is_rejected() -> Result<(), ProtoError> {
        let mut bytes = vec![0; 256];
        bytes.splice(0..2, 300_u16.to_be_bytes());
        assert!(matches!(
            Padded::from_wire(bytes)?.payload(),
            Err(ProtoError::BadPadding)
        ));
        Ok(())
    }

    proptest! {
        #[test]
        fn payload_round_trips(payload in proptest::collection::vec(any::<u8>(), 0..4_094)) {
            let padded = Padded::pad_smallest(&payload, &SizeClass::MEETUP_FRAME)
                .map_err(|error| TestCaseError::fail(error.to_string()))?;
            let received = Padded::from_wire(padded.into_bytes())
                .map_err(|error| TestCaseError::fail(error.to_string()))?;
            prop_assert_eq!(received.payload().ok(), Some(payload.as_slice()));
        }
    }
}
