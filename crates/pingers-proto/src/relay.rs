// SPDX-License-Identifier: AGPL-3.0-only
//! The push relay's own API (design D, "Push relay API"): `GET /v1/config` and the body of
//! `POST /v1/push`. CBOR like the server API.

use minicbor::Decoder;

use crate::cbor::{self, CborBody, Writer, versioned_cbor};
use crate::{ProtoError, PushBind, PushHandle, PushPlatform, SealedPushBlob, TimeToLive, Urgency};

/// The one-byte id of a push relay HPKE key, as in an OHTTP key config. A push request names
/// the key its blob was sealed to; the id is also bound into the seal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PushRelayKeyId(u8);

impl PushRelayKeyId {
    /// Wraps a key id.
    #[must_use]
    pub const fn new(id: u8) -> Self {
        Self(id)
    }

    /// The id byte.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// A push relay's published key in the RFC 9458 §3 key config encoding:
/// `key_id (1) || kem_id (2) || public_key (32) || be16(4) || kdf_id (2) || aead_id (2)`,
/// 41 bytes. The suite is fixed: DHKEM(X25519, HKDF-SHA256) `0x0020`, HKDF-SHA256 `0x0001`,
/// AES-128-GCM `0x0001`. SHA-256 of these bytes is the hash members pin (design B.4.9).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PushRelayKeyConfig {
    /// Which key this is.
    pub key_id: PushRelayKeyId,
    /// The X25519 public key.
    pub public_key: [u8; 32],
}

impl PushRelayKeyConfig {
    /// Encoded length.
    pub const LENGTH: usize = 41;
    /// Everything after the key id and before the public key: the KEM id.
    const KEM_X25519: [u8; 2] = [0x00, 0x20];
    /// Everything after the public key: suites length 4, HKDF-SHA256, AES-128-GCM.
    const SUITES: [u8; 6] = [0x00, 0x04, 0x00, 0x01, 0x00, 0x01];

    /// The 41-byte encoding.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; Self::LENGTH] {
        let mut encoded = [0; Self::LENGTH];
        let fields = [self.key_id.get()]
            .into_iter()
            .chain(Self::KEM_X25519)
            .chain(self.public_key)
            .chain(Self::SUITES);
        for (target, source) in encoded.iter_mut().zip(fields) {
            *target = source;
        }
        encoded
    }

    /// Parses a key config.
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
                field: "push relay key config suite",
            });
        }
        Ok(Self {
            key_id: PushRelayKeyId::new(key_id),
            public_key: *public_key,
        })
    }
}

/// `POST /v1/push` (design D, `PushReq`): a Pingers server asks the relay to wake the device
/// behind one sealed push.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushRequest {
    /// Which relay key the blob is sealed to.
    pub key_id: PushRelayKeyId,
    /// The sealed token.
    pub blob: SealedPushBlob,
    /// The bind of the mailbox or subscription; the relay never sees the id.
    pub bind: PushBind,
    /// Priority for class Standard.
    pub urgency: Urgency,
    /// Echoed in the push so the device knows what to read.
    pub handle: PushHandle,
}

/// Layout: `[key id, blob (320 bytes), bind (32 bytes), urgency, handle (8 bytes)]`.
impl CborBody for PushRequest {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(5)?;
        writer.unsigned(self.key_id.get());
        writer.bytes(self.blob.as_bytes())?;
        writer.bytes(self.bind.as_bytes())?;
        self.urgency.encode_cbor(writer);
        writer.bytes(self.handle.as_bytes())
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 5)?;
        Ok(Self {
            key_id: PushRelayKeyId::new(decoder.u8()?),
            blob: SealedPushBlob::from_bytes(cbor::fixed_bytes(decoder)?),
            bind: PushBind::from_bytes(cbor::fixed_bytes(decoder)?),
            urgency: Urgency::decode_cbor(decoder)?,
            handle: PushHandle::from_bytes(cbor::fixed_bytes(decoder)?),
        })
    }
}

/// `GET /v1/config` (design D, `RelayConfig`): what devices fetch through the OHTTP relay and
/// pin by the SHA-256 of `key_config`'s encoding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushRelayConfig {
    /// The key devices seal to now.
    pub key_config: PushRelayKeyConfig,
    /// The platforms the relay delivers on.
    pub platforms: Vec<PushPlatform>,
    /// The longest lifetime a device should seal into a token.
    pub max_time_to_live: TimeToLive,
    /// Who runs the relay.
    pub operator: String,
}

/// Layout: `[key config (41 bytes, RFC 9458), [platform...], max ttl seconds, operator]`,
/// platforms as in the push token plaintext (1 FCM, 2 `UnifiedPush`).
impl CborBody for PushRelayConfig {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(4)?;
        writer.bytes(&self.key_config.to_bytes())?;
        writer.array(self.platforms.len())?;
        for platform in &self.platforms {
            writer.unsigned(platform.to_wire());
        }
        self.max_time_to_live.encode_cbor(writer)?;
        writer.text(&self.operator)
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 4)?;
        let key_config = PushRelayKeyConfig::from_bytes(decoder.bytes()?)?;
        let count = cbor::array(decoder)?;
        // No preallocation: the count comes from the wire.
        let mut platforms = Vec::new();
        for _ in 0..count {
            platforms.push(PushPlatform::from_wire(decoder.u8()?)?);
        }
        Ok(Self {
            key_config,
            platforms,
            max_time_to_live: TimeToLive::decode_cbor(decoder)?,
            operator: decoder.str()?.to_owned(),
        })
    }
}

versioned_cbor!(PushRequest, PushRelayConfig);

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    fn config(key_id: u8) -> PushRelayKeyConfig {
        PushRelayKeyConfig {
            key_id: PushRelayKeyId::new(key_id),
            public_key: [0x85; 32],
        }
    }

    #[test]
    fn key_config_encoding_follows_rfc_9458() {
        let mut expected = vec![7, 0x00, 0x20];
        expected.extend_from_slice(&[0x85; 32]);
        expected.extend_from_slice(&[0x00, 0x04, 0x00, 0x01, 0x00, 0x01]);
        assert_eq!(config(7).to_bytes().as_slice(), expected.as_slice());
    }

    #[test]
    fn key_config_round_trips() -> Result<(), ProtoError> {
        assert_eq!(
            PushRelayKeyConfig::from_bytes(&config(3).to_bytes())?,
            config(3)
        );
        Ok(())
    }

    #[test]
    fn key_config_with_another_suite_is_refused() {
        // Each of KEM, suites length, KDF and AEAD.
        for position in [2, 36, 38, 40] {
            let mut bytes = config(1).to_bytes();
            if let Some(byte) = bytes.get_mut(position) {
                *byte ^= 0x01;
            }
            assert!(
                matches!(
                    PushRelayKeyConfig::from_bytes(&bytes),
                    Err(ProtoError::OutOfRange {
                        field: "push relay key config suite"
                    })
                ),
                "byte {position}"
            );
        }
    }

    #[test]
    fn key_config_of_the_wrong_length_is_refused() {
        let bytes = config(1).to_bytes();
        assert!(matches!(
            PushRelayKeyConfig::from_bytes(bytes.get(..40).unwrap_or_default()),
            Err(ProtoError::WrongLength {
                expected: 41,
                actual: 40
            })
        ));
    }
}
