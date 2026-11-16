// SPDX-License-Identifier: AGPL-3.0-only
//! The push relay's own API (design D, "Push relay API"): `GET /v1/config` and the body of
//! `POST /v1/push`. CBOR like the server API.

use minicbor::Decoder;
use pingers_time::UnixSeconds;

use crate::cbor::{self, CborBody, Writer, versioned_cbor};
use crate::{
    ProtoError, PushBind, PushHandle, PushPlatform, SealedPushBlob, SingleSuiteKeyConfig,
    TimeToLive, Urgency,
};

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
    pub const LENGTH: usize = SingleSuiteKeyConfig::LENGTH;

    /// The 41-byte encoding, written by [`SingleSuiteKeyConfig`] like the OHTTP gateway's.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; Self::LENGTH] {
        SingleSuiteKeyConfig {
            key_id: self.key_id.get(),
            public_key: self.public_key,
        }
        .to_bytes()
    }

    /// Parses a key config.
    ///
    /// # Errors
    ///
    /// As [`SingleSuiteKeyConfig::from_bytes`].
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ProtoError> {
        let config = SingleSuiteKeyConfig::from_bytes(bytes)?;
        Ok(Self {
            key_id: PushRelayKeyId::new(config.key_id),
            public_key: config.public_key,
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

/// How long a replaced push relay key keeps opening blobs after its successor activates: the
/// longest a sealed token lives (30 days, design B.4.9). The relay refuses a schedule that
/// retires a key sooner; devices cap a token sealed to a key with a successor at that
/// successor's activation plus this, and re-seal to the successor within it.
pub const PUSH_RELAY_KEY_OVERLAP: core::time::Duration = core::time::Duration::from_hours(30 * 24);

/// `GET /v1/config/next` (design B.4.9, C.1a): the key the relay publishes next, and from when.
/// Devices check it against their pins before it activates and switch to it at
/// `activates_at`; the key it replaces keeps opening blobs for [`PUSH_RELAY_KEY_OVERLAP`] after
/// that, so no wake is missed across a rotation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduledPushRelayConfig {
    /// What `GET /v1/config` will serve once the key activates.
    pub config: PushRelayConfig,
    /// When the relay starts publishing the key and opening blobs sealed to it.
    pub activates_at: UnixSeconds,
}

/// Layout: `[config (as PushRelayConfig), activates at (Unix seconds)]`.
impl CborBody for ScheduledPushRelayConfig {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(2)?;
        self.config.encode_body(writer)?;
        writer.unsigned(self.activates_at.get());
        Ok(())
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 2)?;
        Ok(Self {
            config: PushRelayConfig::decode_body(decoder)?,
            activates_at: UnixSeconds::new(decoder.u64()?),
        })
    }
}

versioned_cbor!(PushRequest, PushRelayConfig, ScheduledPushRelayConfig);

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

    // The encoding itself is tested in `key_config`; these pin the key id mapping.
    #[test]
    fn key_config_is_the_single_suite_encoding_with_the_key_id_first() {
        let expected = SingleSuiteKeyConfig {
            key_id: 7,
            public_key: [0x85; 32],
        }
        .to_bytes();
        assert_eq!(config(7).to_bytes(), expected);
    }

    #[test]
    fn key_config_round_trips() -> Result<(), ProtoError> {
        assert_eq!(
            PushRelayKeyConfig::from_bytes(&config(3).to_bytes())?,
            config(3)
        );
        Ok(())
    }
}

#[cfg(test)]
mod overlap_tests {
    #[test]
    fn the_key_overlap_is_thirty_days() {
        assert_eq!(super::PUSH_RELAY_KEY_OVERLAP.as_secs(), 30 * 24 * 60 * 60);
    }
}
