// SPDX-License-Identifier: AGPL-3.0-only
//! The canonical CBOR layout shared by every variable-size wire type.
//!
//! Every top-level message is the two-element array `[version, body]`, where `version` is
//! [`VERSION`] and `body` is the type's fields as a definite-length array in declaration order.
//! Nested structures are bare bodies (no version). Conventions:
//!
//! - integers in their shortest form; fixed-size byte arrays as byte strings of exactly that
//!   length; text as UTF-8 text strings;
//! - `Option::None` as CBOR `null`, `Some(value)` as the value itself;
//! - two-state enums as small unsigned integers; enums with data as `[variant, fields...]`;
//! - `f32` as a four-byte float (major type 7, `0xfa`).
//!
//! The encoder below is written by hand so that it can only produce that canonical form and
//! cannot fail. The decoder uses `minicbor` for the parsing and then re-encodes the value and
//! compares: any input that is not byte-for-byte the canonical encoding (long-form integers,
//! indefinite lengths, half-precision floats) is rejected with [`ProtoError::NonCanonical`].

use minicbor::Decoder;
use minicbor::data::Type;

use crate::ProtoError;

/// The only layout version this implementation reads and writes.
pub(crate) const VERSION: u64 = 1;

const MAJOR_UNSIGNED: u8 = 0x00;
const MAJOR_NEGATIVE: u8 = 0x20;
const MAJOR_BYTES: u8 = 0x40;
const MAJOR_TEXT: u8 = 0x60;
const MAJOR_ARRAY: u8 = 0x80;
const NULL: u8 = 0xf6;
const FLOAT32: u8 = 0xfa;
/// Additional-information values 0 to 23 are the value itself.
const LARGEST_IMMEDIATE: u8 = 23;
const FOLLOWS_U8: u8 = 24;
const FOLLOWS_U16: u8 = 25;
const FOLLOWS_U32: u8 = 26;
const FOLLOWS_U64: u8 = 27;

/// Builds a canonical CBOR encoding. Writing to a `Vec` cannot fail.
#[derive(Default)]
pub(crate) struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    /// The bytes written so far.
    pub(crate) fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    fn head(&mut self, major: u8, value: u64) {
        if let Ok(small) = u8::try_from(value) {
            if small <= LARGEST_IMMEDIATE {
                self.bytes.push(major | small);
            } else {
                self.bytes.extend_from_slice(&[major | FOLLOWS_U8, small]);
            }
        } else if let Ok(medium) = u16::try_from(value) {
            self.bytes.push(major | FOLLOWS_U16);
            self.bytes.extend_from_slice(&medium.to_be_bytes());
        } else if let Ok(large) = u32::try_from(value) {
            self.bytes.push(major | FOLLOWS_U32);
            self.bytes.extend_from_slice(&large.to_be_bytes());
        } else {
            self.bytes.push(major | FOLLOWS_U64);
            self.bytes.extend_from_slice(&value.to_be_bytes());
        }
    }

    fn length(&mut self, major: u8, length: usize) -> Result<(), ProtoError> {
        let length = u64::try_from(length).map_err(|_overflow| ProtoError::OutOfRange {
            field: "cbor length",
        })?;
        self.head(major, length);
        Ok(())
    }

    /// An unsigned integer.
    pub(crate) fn unsigned(&mut self, value: impl Into<u64>) {
        self.head(MAJOR_UNSIGNED, value.into());
    }

    /// A signed integer.
    pub(crate) fn signed(&mut self, value: i32) {
        match u32::try_from(value) {
            Ok(positive) => self.head(MAJOR_UNSIGNED, u64::from(positive)),
            // CBOR stores a negative `n` as `-1 - n`, which is `|n| - 1` and cannot saturate.
            Err(_negative) => self.head(
                MAJOR_NEGATIVE,
                u64::from(value.unsigned_abs().saturating_sub(1)),
            ),
        }
    }

    /// A byte string.
    pub(crate) fn bytes(&mut self, value: &[u8]) -> Result<(), ProtoError> {
        self.length(MAJOR_BYTES, value.len())?;
        self.bytes.extend_from_slice(value);
        Ok(())
    }

    /// A text string.
    pub(crate) fn text(&mut self, value: &str) -> Result<(), ProtoError> {
        self.length(MAJOR_TEXT, value.len())?;
        self.bytes.extend_from_slice(value.as_bytes());
        Ok(())
    }

    /// The header of a definite-length array of `length` elements.
    pub(crate) fn array(&mut self, length: usize) -> Result<(), ProtoError> {
        self.length(MAJOR_ARRAY, length)
    }

    /// `null`, for `Option::None`.
    pub(crate) fn null(&mut self) {
        self.bytes.push(NULL);
    }

    /// A four-byte float.
    pub(crate) fn float(&mut self, value: f32) {
        self.bytes.push(FLOAT32);
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    /// An optional value: `null`, or the value as `write` encodes it.
    pub(crate) fn option<T>(
        &mut self,
        value: Option<&T>,
        write: impl FnOnce(&mut Self, &T) -> Result<(), ProtoError>,
    ) -> Result<(), ProtoError> {
        if let Some(inner) = value {
            write(self, inner)
        } else {
            self.null();
            Ok(())
        }
    }

    /// An array of nested bodies.
    pub(crate) fn list<T: CborBody>(&mut self, items: &[T]) -> Result<(), ProtoError> {
        self.array(items.len())?;
        items.iter().try_for_each(|item| item.encode_body(self))
    }
}

impl From<minicbor::decode::Error> for ProtoError {
    fn from(error: minicbor::decode::Error) -> Self {
        Self::Cbor(error)
    }
}

/// Reads the header of a definite-length array and checks it has `expected` elements.
pub(crate) fn array_of(decoder: &mut Decoder<'_>, expected: u64) -> Result<(), ProtoError> {
    expect_length(array(decoder)?, expected)
}

/// Checks an array's element count, for enums whose length depends on the variant.
pub(crate) fn expect_length(actual: u64, expected: u64) -> Result<(), ProtoError> {
    if actual == expected {
        Ok(())
    } else {
        Err(ProtoError::Cbor(minicbor::decode::Error::message(
            "array has the wrong number of elements",
        )))
    }
}

/// Reads the header of a definite-length array and returns its element count.
pub(crate) fn array(decoder: &mut Decoder<'_>) -> Result<u64, ProtoError> {
    decoder.array()?.ok_or(ProtoError::NonCanonical)
}

/// Reads a byte string that must be exactly `LENGTH` bytes long.
pub(crate) fn fixed_bytes<const LENGTH: usize>(
    decoder: &mut Decoder<'_>,
) -> Result<[u8; LENGTH], ProtoError> {
    let bytes = decoder.bytes()?;
    bytes.try_into().map_err(|_length| ProtoError::WrongLength {
        expected: LENGTH,
        actual: bytes.len(),
    })
}

/// Reads `null` as `None`, or anything else through `read`.
pub(crate) fn option<'bytes, T>(
    decoder: &mut Decoder<'bytes>,
    read: impl FnOnce(&mut Decoder<'bytes>) -> Result<T, ProtoError>,
) -> Result<Option<T>, ProtoError> {
    if decoder.datatype()? == Type::Null {
        decoder.null()?;
        Ok(None)
    } else {
        read(decoder).map(Some)
    }
}

/// Reads an array of nested bodies.
pub(crate) fn list<T: CborBody>(decoder: &mut Decoder<'_>) -> Result<Vec<T>, ProtoError> {
    let count = array(decoder)?;
    // No preallocation: the count comes from the wire.
    let mut items = Vec::new();
    for _ in 0..count {
        items.push(T::decode_body(decoder)?);
    }
    Ok(items)
}

/// A structure with a CBOR body: the part of the layout without the version.
pub(crate) trait CborBody: Sized {
    /// Writes the body.
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError>;

    /// Reads the body. The caller checks for trailing bytes and canonical form.
    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError>;
}

/// The top-level encoding `[VERSION, body]`.
pub(crate) fn encode_versioned<T: CborBody>(value: &T) -> Result<Vec<u8>, ProtoError> {
    let mut writer = Writer::default();
    writer.array(2)?;
    writer.unsigned(VERSION);
    value.encode_body(&mut writer)?;
    Ok(writer.into_bytes())
}

/// Parses `[VERSION, body]` and accepts it only if it is exactly the canonical encoding.
pub(crate) fn decode_versioned<T: CborBody>(bytes: &[u8]) -> Result<T, ProtoError> {
    let mut decoder = Decoder::new(bytes);
    array_of(&mut decoder, 2)?;
    let version = decoder.u64()?;
    if version != VERSION {
        return Err(ProtoError::UnsupportedVersion { version });
    }
    let value = T::decode_body(&mut decoder)?;
    if decoder.position() != bytes.len() {
        return Err(ProtoError::TrailingBytes);
    }
    if encode_versioned(&value)? != bytes {
        return Err(ProtoError::NonCanonical);
    }
    Ok(value)
}

/// Implements [`crate::WireEncode`] and [`crate::WireDecode`] through [`CborBody`] with the
/// versioned top-level layout.
macro_rules! versioned_cbor {
    ($($name:ty),* $(,)?) => {
        $(
            impl $crate::WireEncode for $name {
                fn encode(&self) -> Result<Vec<u8>, $crate::ProtoError> {
                    $crate::cbor::encode_versioned(self)
                }
            }

            impl $crate::WireDecode for $name {
                fn decode(bytes: &[u8]) -> Result<Self, $crate::ProtoError> {
                    $crate::cbor::decode_versioned(bytes)
                }
            }
        )*
    };
}

pub(crate) use versioned_cbor;

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    fn encoded(write: impl FnOnce(&mut Writer)) -> Vec<u8> {
        let mut writer = Writer::default();
        write(&mut writer);
        writer.into_bytes()
    }

    #[test]
    fn unsigned_integers_use_the_shortest_head() {
        let cases: [(u64, &[u8]); 9] = [
            (0, &[0x00]),
            (23, &[0x17]),
            (24, &[0x18, 24]),
            (255, &[0x18, 0xff]),
            (256, &[0x19, 0x01, 0x00]),
            (65_535, &[0x19, 0xff, 0xff]),
            (65_536, &[0x1a, 0x00, 0x01, 0x00, 0x00]),
            (u64::from(u32::MAX), &[0x1a, 0xff, 0xff, 0xff, 0xff]),
            (
                u64::from(u32::MAX).saturating_add(1),
                &[0x1b, 0, 0, 0, 1, 0, 0, 0, 0],
            ),
        ];
        for (value, expected) in cases {
            assert_eq!(
                encoded(|writer| writer.unsigned(value)),
                expected,
                "{value}"
            );
        }
    }

    #[test]
    fn signed_integers_match_rfc_8949_examples() {
        let cases: [(i32, &[u8]); 6] = [
            (0, &[0x00]),
            (10, &[0x0a]),
            (-1, &[0x20]),
            (-10, &[0x29]),
            (-100, &[0x38, 0x63]),
            (-1_000, &[0x39, 0x03, 0xe7]),
        ];
        for (value, expected) in cases {
            assert_eq!(encoded(|writer| writer.signed(value)), expected, "{value}");
        }
        assert_eq!(
            encoded(|writer| writer.signed(i32::MIN)),
            [0x3a, 0x7f, 0xff, 0xff, 0xff]
        );
    }

    #[test]
    fn signed_integers_decode_back() -> Result<(), ProtoError> {
        for value in [i32::MIN, -1_800_000_000, -24, -1, 0, 1, i32::MAX] {
            let bytes = encoded(|writer| writer.signed(value));
            assert_eq!(Decoder::new(&bytes).i32()?, value);
        }
        Ok(())
    }

    #[test]
    fn strings_and_arrays_carry_their_length() -> Result<(), ProtoError> {
        let mut writer = Writer::default();
        writer.array(3)?;
        writer.bytes(&[1, 2])?;
        writer.text("a")?;
        writer.null();
        assert_eq!(writer.into_bytes(), [0x83, 0x42, 1, 2, 0x61, b'a', 0xf6]);
        Ok(())
    }

    #[test]
    fn floats_are_four_bytes() {
        assert_eq!(
            encoded(|writer| writer.float(1.0)),
            [0xfa, 0x3f, 0x80, 0x00, 0x00]
        );
    }

    #[test]
    fn indefinite_arrays_are_not_canonical() {
        let bytes = [0x9f, 0xff];
        assert!(matches!(
            array(&mut Decoder::new(&bytes)),
            Err(ProtoError::NonCanonical)
        ));
    }

    #[test]
    fn fixed_bytes_report_the_length_they_got() {
        let bytes = [0x42, 1, 2];
        assert!(matches!(
            fixed_bytes::<3>(&mut Decoder::new(&bytes)),
            Err(ProtoError::WrongLength {
                expected: 3,
                actual: 2
            })
        ));
    }
}
