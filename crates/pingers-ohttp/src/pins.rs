// SPDX-License-Identifier: AGPL-3.0-only
//! Pin sets: the key config hashes a key config may match (design B.12, two-slot rotation).
//!
//! A build pins both slots of a key: the one served now and the next one, published before it
//! is served. The app then never meets an unpinned key when the operator rotates. The text form
//! is the one deployment tooling prints (`cargo xtask pins`, the `pins` subcommands) and CI
//! stores: each hash as base64url (RFC 4648 §5, padding optional), comma-separated.

use base64::Engine as _;
use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig};
use base64::engine::{DecodePaddingMode, general_purpose};
use pingers_proto::KeyConfigHash;

use crate::{KeyConfig, OhttpError};

/// base64url that accepts a pin with or without its one `=`.
const PIN_TEXT: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::URL_SAFE,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// One or more pinned key config hashes, without repeats.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyPins(Vec<KeyConfigHash>);

impl KeyPins {
    /// The pins, in the order given, repeats dropped.
    ///
    /// # Errors
    ///
    /// [`OhttpError::InvalidPin`] if `hashes` is empty: a set that admits nothing would stop
    /// all traffic.
    pub fn new(hashes: impl IntoIterator<Item = KeyConfigHash>) -> Result<Self, OhttpError> {
        let mut pins: Vec<KeyConfigHash> = Vec::new();
        for hash in hashes {
            if !pins.contains(&hash) {
                pins.push(hash);
            }
        }
        if pins.is_empty() {
            return Err(OhttpError::InvalidPin);
        }
        Ok(Self(pins))
    }

    /// Parses the text form: base64url hashes separated by commas, spaces around each allowed.
    ///
    /// # Errors
    ///
    /// [`OhttpError::InvalidPin`] if an entry is empty, is not base64url, or is not 32 bytes.
    pub fn parse(text: &str) -> Result<Self, OhttpError> {
        Self::new(
            text.split(',')
                .map(|entry| parse_one(entry.trim()))
                .collect::<Result<Vec<_>, _>>()?,
        )
    }

    /// The text form, unpadded.
    #[must_use]
    pub fn to_text(&self) -> String {
        self.0
            .iter()
            .map(|hash| general_purpose::URL_SAFE_NO_PAD.encode(hash.as_bytes()))
            .collect::<Vec<_>>()
            .join(",")
    }

    /// Whether `hash` is pinned.
    #[must_use]
    pub fn contains(&self, hash: &KeyConfigHash) -> bool {
        self.0.contains(hash)
    }

    /// The pinned hashes.
    #[must_use]
    pub fn hashes(&self) -> &[KeyConfigHash] {
        &self.0
    }
}

impl From<KeyConfigHash> for KeyPins {
    fn from(hash: KeyConfigHash) -> Self {
        Self(vec![hash])
    }
}

fn parse_one(entry: &str) -> Result<KeyConfigHash, OhttpError> {
    let bytes = PIN_TEXT
        .decode(entry)
        .map_err(|_not_base64url| OhttpError::InvalidPin)?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_wrong_length| OhttpError::InvalidPin)?;
    Ok(KeyConfigHash::from_bytes(bytes))
}

/// Checks a fetched config against a pin set.
///
/// # Errors
///
/// [`OhttpError::KeyMismatch`] if its hash is not in the set.
pub fn check_pins(config: &KeyConfig, pins: &KeyPins) -> Result<(), OhttpError> {
    if pins.contains(&config.hash()) {
        Ok(())
    } else {
        Err(OhttpError::KeyMismatch)
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    fn hash(byte: u8) -> KeyConfigHash {
        KeyConfigHash::from_bytes([byte; 32])
    }

    #[test]
    fn the_text_form_is_comma_separated_base64url() -> Result<(), OhttpError> {
        // 0xfb bytes need the URL-safe alphabet: standard base64 would write `+` and `/`.
        let pins = KeyPins::new([hash(0xfb), hash(0xff)])?;
        let text = pins.to_text();
        assert_eq!(
            text,
            "-_v7-_v7-_v7-_v7-_v7-_v7-_v7-_v7-_v7-_v7-_s,\
             __________________________________________8"
        );
        assert_eq!(KeyPins::parse(&text)?, pins);
        Ok(())
    }

    #[test]
    fn padding_and_spaces_are_accepted() -> Result<(), OhttpError> {
        let text = " -_v7-_v7-_v7-_v7-_v7-_v7-_v7-_v7-_v7-_v7-_s= , \
                    __________________________________________8";
        assert_eq!(KeyPins::parse(text)?, KeyPins::new([hash(0xfb), hash(0xff)])?);
        Ok(())
    }

    #[test]
    fn repeats_are_dropped_and_order_is_kept() -> Result<(), OhttpError> {
        let pins = KeyPins::new([hash(2), hash(1), hash(2)])?;
        assert_eq!(pins.hashes(), [hash(2), hash(1)]);
        Ok(())
    }

    #[test]
    fn bad_pin_text_is_refused() {
        for text in [
            "",
            ",",
            "AAAA",
            // Standard base64, not base64url.
            "+/v7+/v7+/v7+/v7+/v7+/v7+/v7+/v7+/v7+/v7+/s",
            // 33 bytes.
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        ] {
            assert!(
                matches!(KeyPins::parse(text), Err(OhttpError::InvalidPin)),
                "{text:?}"
            );
        }
        assert!(matches!(
            KeyPins::new(Vec::new()),
            Err(OhttpError::InvalidPin)
        ));
    }

    #[test]
    fn only_pinned_hashes_are_contained() -> Result<(), OhttpError> {
        let pins = KeyPins::new([hash(1), hash(2)])?;
        assert!(pins.contains(&hash(1)));
        assert!(pins.contains(&hash(2)));
        assert!(!pins.contains(&hash(3)));
        assert_eq!(KeyPins::from(hash(3)).hashes(), [hash(3)]);
        Ok(())
    }
}
