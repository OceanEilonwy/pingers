// SPDX-License-Identifier: AGPL-3.0-only
//! A canonical CBOR writer (RFC 8949 §4.2.1: shortest heads, definite lengths), written
//! independently of pingers-proto so the vectors check its encoder rather than repeat it.
//! Only the items the wire formats use: unsigned and negative integers, byte and text strings,
//! arrays and null.

/// Major type 0, unsigned integer.
const UNSIGNED: u8 = 0x00;
/// Major type 1, negative integer `-1 - n`.
const NEGATIVE: u8 = 0x20;
/// Major type 2, byte string.
const BYTES: u8 = 0x40;
/// Major type 3, UTF-8 text string.
const TEXT: u8 = 0x60;
/// Major type 4, array.
const ARRAY: u8 = 0x80;

/// The simple value null.
pub(crate) const NULL: [u8; 1] = [0xf6];

/// A head with its argument in the fewest bytes.
fn head(major: u8, argument: u64) -> Vec<u8> {
    if let Ok(small) = u8::try_from(argument) {
        if small < 24 {
            return vec![major | small];
        }
        return vec![major | 0x18, small];
    }
    if let Ok(short) = u16::try_from(argument) {
        return [&[major | 0x19][..], &short.to_be_bytes()].concat();
    }
    if let Ok(word) = u32::try_from(argument) {
        return [&[major | 0x1a][..], &word.to_be_bytes()].concat();
    }
    [&[major | 0x1b][..], &argument.to_be_bytes()].concat()
}

/// A length as a head argument.
fn length(len: usize) -> u64 {
    // usize is at most 64 bits on every target Rust supports.
    u64::try_from(len).unwrap_or(u64::MAX)
}

/// An unsigned integer.
pub(crate) fn uint(value: u64) -> Vec<u8> {
    head(UNSIGNED, value)
}

/// A signed integer: major type 0 when non-negative, else major type 1 with `-1 - value`,
/// which is `!value` in two's complement.
pub(crate) fn int(value: i64) -> Vec<u8> {
    if value < 0 {
        head(NEGATIVE, (!value).unsigned_abs())
    } else {
        head(UNSIGNED, value.unsigned_abs())
    }
}

/// A byte string.
pub(crate) fn bytes(data: &[u8]) -> Vec<u8> {
    [head(BYTES, length(data.len())), data.to_vec()].concat()
}

/// A text string.
pub(crate) fn text(value: &str) -> Vec<u8> {
    [head(TEXT, length(value.len())), value.as_bytes().to_vec()].concat()
}

/// An array of already-encoded items.
pub(crate) fn array<const N: usize>(items: [Vec<u8>; N]) -> Vec<u8> {
    array_of(Vec::from(items))
}

/// An array whose length is only known at run time.
pub(crate) fn array_of(items: Vec<Vec<u8>>) -> Vec<u8> {
    let mut encoding = head(ARRAY, length(items.len()));
    for item in items {
        encoding.extend(item);
    }
    encoding
}

/// An eight-byte float (`0xfb`), never shortened: Spot geometry round-trips bit for bit.
pub(crate) fn double(value: f64) -> Vec<u8> {
    [vec![0xfb], value.to_be_bytes().to_vec()].concat()
}

/// `value` encoded, or null when absent.
pub(crate) fn optional<T>(value: Option<T>, encode: impl FnOnce(T) -> Vec<u8>) -> Vec<u8> {
    value.map_or_else(|| NULL.to_vec(), encode)
}

/// Every wire message's outer shape: `[1, body]`.
pub(crate) fn versioned(body: Vec<u8>) -> Vec<u8> {
    array([uint(1), body])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heads_take_the_fewest_bytes() {
        assert_eq!(uint(23), [0x17]);
        assert_eq!(uint(24), [0x18, 24]);
        assert_eq!(uint(255), [0x18, 0xff]);
        assert_eq!(uint(256), [0x19, 0x01, 0x00]);
        assert_eq!(uint(65_535), [0x19, 0xff, 0xff]);
        assert_eq!(uint(65_536), [0x1a, 0, 1, 0, 0]);
        assert_eq!(uint(u64::from(u32::MAX)), [0x1a, 0xff, 0xff, 0xff, 0xff]);
        assert_eq!(uint(4_294_967_296), [0x1b, 0, 0, 0, 1, 0, 0, 0, 0]);
    }

    #[test]
    fn negative_integers_store_minus_one_minus_the_value() {
        assert_eq!(int(-1), [0x20]);
        assert_eq!(int(-24), [0x37]);
        assert_eq!(int(-25), [0x38, 24]);
        assert_eq!(int(0), [0x00]);
        assert_eq!(
            int(i64::MIN),
            [&[0x3b][..], &i64::MAX.to_be_bytes()].concat()
        );
    }

    #[test]
    fn strings_arrays_and_null() {
        assert_eq!(bytes(&[1, 2]), [0x42, 1, 2]);
        assert_eq!(text("é"), [0x62, 0xc3, 0xa9]);
        assert_eq!(array([uint(7), NULL.to_vec()]), [0x82, 0x07, 0xf6]);
        assert_eq!(optional(None::<u64>, uint), NULL);
        assert_eq!(optional(Some(5), uint), [0x05]);
        assert_eq!(versioned(array([uint(7)])), [0x82, 0x01, 0x81, 0x07]);
    }
}
