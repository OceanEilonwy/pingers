// SPDX-License-Identifier: AGPL-3.0-only
//! The one RFC 9458 §3 key config encoding Pingers writes. Both the OHTTP gateway's config
//! and the push relay's config use it, and members pin the SHA-256 of these bytes, so there
//! is exactly one encoder.

use crate::ProtoError;

/// An HPKE public key config offering a single suite: DHKEM(X25519, HKDF-SHA256) `0x0020`,
/// HKDF-SHA256 `0x0001`, AES-128-GCM `0x0001` (design B.4.9).
///
/// Encoding: `key_id (1) || kem_id (2) || public_key (32) || be16(4) || kdf_id (2) ||
/// aead_id (2)`, 41 bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SingleSuiteKeyConfig {
    /// The key id requests and seals name.
    pub key_id: u8,
    /// The X25519 public key.
    pub public_key: [u8; 32],
}

impl SingleSuiteKeyConfig {
    /// Encoded length.
    pub const LENGTH: usize = 41;
    /// The KEM id, between the key id and the public key.
    const KEM_X25519: [u8; 2] = [0x00, 0x20];
    /// Everything after the public key: suites length 4, HKDF-SHA256, AES-128-GCM.
    const SUITES: [u8; 6] = [0x00, 0x04, 0x00, 0x01, 0x00, 0x01];

    /// The 41-byte encoding.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; Self::LENGTH] {
        let mut encoded = [0; Self::LENGTH];
        let fields = [self.key_id]
            .into_iter()
            .chain(Self::KEM_X25519)
            .chain(self.public_key)
            .chain(Self::SUITES);
        for (target, source) in encoded.iter_mut().zip(fields) {
            *target = source;
        }
        encoded
    }

    /// Parses a single-suite key config.
    ///
    /// # Errors
    ///
    /// [`ProtoError::WrongLength`] unless it is 41 bytes; [`ProtoError::OutOfRange`] if the
    /// KEM or the suite is not the one Pingers uses.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ProtoError> {
        let fixed: &[u8; Self::LENGTH] =
            bytes
                .try_into()
                .map_err(|_length| ProtoError::WrongLength {
                    expected: Self::LENGTH,
                    actual: bytes.len(),
                })?;
        let (&[key_id], rest) = fixed
            .split_first_chunk::<1>()
            .ok_or(ProtoError::BadPadding)?;
        let (kem, rest) = rest
            .split_first_chunk::<2>()
            .ok_or(ProtoError::BadPadding)?;
        let (public_key, suites) = rest
            .split_first_chunk::<32>()
            .ok_or(ProtoError::BadPadding)?;
        if *kem != Self::KEM_X25519 || suites != Self::SUITES {
            return Err(ProtoError::OutOfRange {
                field: "key config suite",
            });
        }
        Ok(Self {
            key_id,
            public_key: *public_key,
        })
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    fn config(key_id: u8) -> SingleSuiteKeyConfig {
        SingleSuiteKeyConfig {
            key_id,
            public_key: [0x85; 32],
        }
    }

    #[test]
    fn encoding_follows_rfc_9458() {
        let mut expected = vec![7, 0x00, 0x20];
        expected.extend_from_slice(&[0x85; 32]);
        expected.extend_from_slice(&[0x00, 0x04, 0x00, 0x01, 0x00, 0x01]);
        assert_eq!(config(7).to_bytes().as_slice(), expected.as_slice());
    }

    #[test]
    fn a_config_round_trips() -> Result<(), ProtoError> {
        assert_eq!(
            SingleSuiteKeyConfig::from_bytes(&config(3).to_bytes())?,
            config(3)
        );
        Ok(())
    }

    #[test]
    fn a_config_with_another_suite_is_refused() {
        // Each of KEM, suites length, KDF and AEAD.
        for position in [2, 36, 38, 40] {
            let mut bytes = config(1).to_bytes();
            if let Some(byte) = bytes.get_mut(position) {
                *byte ^= 0x01;
            }
            assert!(
                matches!(
                    SingleSuiteKeyConfig::from_bytes(&bytes),
                    Err(ProtoError::OutOfRange {
                        field: "key config suite"
                    })
                ),
                "byte {position}"
            );
        }
    }

    #[test]
    fn a_config_of_the_wrong_length_is_refused() {
        let bytes = config(1).to_bytes();
        assert!(matches!(
            SingleSuiteKeyConfig::from_bytes(bytes.get(..40).unwrap_or_default()),
            Err(ProtoError::WrongLength {
                expected: 41,
                actual: 40
            })
        ));
    }
}
