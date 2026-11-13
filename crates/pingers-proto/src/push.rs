// SPDX-License-Identifier: AGPL-3.0-only
//! Mailbox registration, sealed push tokens and group subscriptions (design B.4.9, D).

use core::time::Duration;

use minicbor::Decoder;
use pingers_time::UnixSeconds;

use crate::cbor::{self, CborBody, Writer, versioned_cbor};
use crate::{MailboxId, ProtoError, PushHandle, RelayId, SubscriptionId};

/// How long a mailbox or subscription lives on the server: at least one second and at most
/// `u32::MAX` seconds, as the wire's `ttl_s: u32` allows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TimeToLive(Duration);

impl TimeToLive {
    /// Checks that `duration` is representable on the wire (whole seconds are kept).
    ///
    /// # Errors
    ///
    /// [`ProtoError::OutOfRange`] if it is under one second or over `u32::MAX` seconds.
    pub fn new(duration: Duration) -> Result<Self, ProtoError> {
        let seconds = duration.as_secs();
        if seconds == 0 || u32::try_from(seconds).is_err() {
            return Err(ProtoError::OutOfRange { field: "ttl" });
        }
        Ok(Self(Duration::from_secs(seconds)))
    }

    /// The lifetime.
    #[must_use]
    pub const fn duration(self) -> Duration {
        self.0
    }

    /// Writes `ttl_s: u32`; the constructor guarantees it fits.
    fn encode_cbor(self, writer: &mut Writer) -> Result<(), ProtoError> {
        let seconds = u32::try_from(self.0.as_secs())
            .map_err(|_overflow| ProtoError::OutOfRange { field: "ttl" })?;
        writer.unsigned(seconds);
        Ok(())
    }

    fn decode_cbor(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        Self::new(Duration::from_secs(u64::from(decoder.u32()?)))
    }
}

/// How urgently a Standard-class push should reach the device (design B.4.9, B.7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Urgency {
    /// Something the user should see: high-priority data message, app posts a notification.
    Alert,
    /// Housekeeping: normal-priority data message; the server may delay it up to 30 s.
    Quiet,
}

impl Urgency {
    /// Writes the wire value: 0 for Alert, 1 for Quiet.
    pub(crate) fn encode_cbor(self, writer: &mut Writer) {
        writer.unsigned(match self {
            Self::Alert => 0_u8,
            Self::Quiet => 1,
        });
    }

    /// Reads the wire value.
    pub(crate) fn decode_cbor(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        match decoder.u8()? {
            0 => Ok(Self::Alert),
            1 => Ok(Self::Quiet),
            _ => Err(ProtoError::OutOfRange { field: "urgency" }),
        }
    }
}

/// Which push service delivers to the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PushPlatform {
    /// Firebase Cloud Messaging HTTP v1.
    Fcm,
    /// A `UnifiedPush` distributor; the token is the distributor endpoint.
    UnifiedPush,
}

impl PushPlatform {
    const fn to_wire(self) -> u8 {
        match self {
            Self::Fcm => 1,
            Self::UnifiedPush => 2,
        }
    }

    const fn from_wire(value: u8) -> Result<Self, ProtoError> {
        match value {
            1 => Ok(Self::Fcm),
            2 => Ok(Self::UnifiedPush),
            _ => Err(ProtoError::OutOfRange {
                field: "push platform",
            }),
        }
    }
}

/// What the push is for (design B.4.9).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PushClass {
    /// Alerts and housekeeping; [`Urgency`] chooses the priority.
    Standard,
    /// A knock arrived: wake the device for a liveness check (on Android a high-priority FCM
    /// data message). Urgency does not apply.
    Location,
}

impl PushClass {
    const fn to_wire(self) -> u8 {
        match self {
            Self::Standard => 0,
            Self::Location => 1,
        }
    }

    const fn from_wire(value: u8) -> Result<Self, ProtoError> {
        match value {
            0 => Ok(Self::Standard),
            1 => Ok(Self::Location),
            _ => Err(ProtoError::OutOfRange {
                field: "push class",
            }),
        }
    }
}

/// A device's push token or `UnifiedPush` endpoint. Never sent anywhere except sealed to the push
/// relay; `Debug` does not print it.
#[derive(Clone, PartialEq, Eq)]
pub struct PushToken(Vec<u8>);

impl PushToken {
    /// Wraps a token from the platform.
    #[must_use]
    pub const fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// The token bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl core::fmt::Debug for PushToken {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("PushToken(redacted)")
    }
}

/// The plaintext sealed to the push relay: `platform || class || push_token || not_after`
/// (design B.4.9), padded to [`PushTokenPlaintext::PADDED_SIZE`].
///
/// Padded layout: `version (1) || platform (1) || class (1) || be64(not_after) ||
/// be16(token length) || token || zero bytes`. The version is 1; platform 1 is FCM and 2 is
/// `UnifiedPush`; class 0 is Standard and 1 is Location. The token is at most
/// [`PushTokenPlaintext::MAX_TOKEN_BYTES`] long.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushTokenPlaintext {
    /// The push service.
    pub platform: PushPlatform,
    /// Standard or Location.
    pub class: PushClass,
    /// The token.
    pub token: PushToken,
    /// After this the relay drops the blob.
    pub not_after: UnixSeconds,
}

impl PushTokenPlaintext {
    /// Padded plaintext size: the 320-byte blob minus the X25519 encapsulated key (32) and the
    /// AES-128-GCM tag (16).
    pub const PADDED_SIZE: usize = 272;

    /// Bytes before the token: version, platform, class, `not_after` and the token length.
    const HEADER: usize = 13;

    /// The longest token that fits.
    pub const MAX_TOKEN_BYTES: usize = Self::PADDED_SIZE - Self::HEADER;

    /// The layout version byte.
    const VERSION: u8 = 1;

    /// The padded plaintext for pingers-crypto to seal.
    ///
    /// # Errors
    ///
    /// [`ProtoError::OutOfRange`] if the token is longer than
    /// [`PushTokenPlaintext::MAX_TOKEN_BYTES`].
    pub fn encode_padded(&self) -> Result<[u8; Self::PADDED_SIZE], ProtoError> {
        let token = self.token.as_bytes();
        let too_long = ProtoError::OutOfRange {
            field: "push token",
        };
        if token.len() > Self::MAX_TOKEN_BYTES {
            return Err(too_long);
        }
        let token_length = u16::try_from(token.len()).map_err(|_overflow| too_long)?;
        let mut fields = Vec::with_capacity(Self::PADDED_SIZE);
        fields.extend_from_slice(&[Self::VERSION, self.platform.to_wire(), self.class.to_wire()]);
        fields.extend_from_slice(&self.not_after.get().to_be_bytes());
        fields.extend_from_slice(&token_length.to_be_bytes());
        fields.extend_from_slice(token);
        fields.resize(Self::PADDED_SIZE, 0);
        fields.try_into().map_err(|_length| ProtoError::OutOfRange {
            field: "push token",
        })
    }

    /// Parses an opened plaintext (push relay side).
    ///
    /// # Errors
    ///
    /// [`ProtoError::UnsupportedVersion`] for a version other than 1;
    /// [`ProtoError::OutOfRange`] for an unknown platform or class; [`ProtoError::BadPadding`]
    /// if the token length runs past the end or a padding byte is not zero.
    pub fn decode_padded(bytes: &[u8; Self::PADDED_SIZE]) -> Result<Self, ProtoError> {
        let (&[version, platform, class], rest) = bytes
            .split_first_chunk::<3>()
            .ok_or(ProtoError::BadPadding)?;
        if version != Self::VERSION {
            return Err(ProtoError::UnsupportedVersion {
                version: u64::from(version),
            });
        }
        let platform = PushPlatform::from_wire(platform)?;
        let class = PushClass::from_wire(class)?;
        let (not_after, rest) = rest
            .split_first_chunk::<8>()
            .ok_or(ProtoError::BadPadding)?;
        let (token_length, rest) = rest
            .split_first_chunk::<2>()
            .ok_or(ProtoError::BadPadding)?;
        let (token, padding) = rest
            .split_at_checked(usize::from(u16::from_be_bytes(*token_length)))
            .ok_or(ProtoError::BadPadding)?;
        if padding.iter().any(|byte| *byte != 0) {
            return Err(ProtoError::BadPadding);
        }
        Ok(Self {
            platform,
            class,
            token: PushToken::from_bytes(token.to_vec()),
            not_after: UnixSeconds::new(u64::from_be_bytes(*not_after)),
        })
    }
}

/// `HPKE.Seal(push_relay_pk, info = "pingers/v1/push" || bind, padded plaintext)`: 320 bytes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SealedPushBlob([u8; 320]);

impl SealedPushBlob {
    /// Size in bytes.
    pub const SIZE: usize = 320;

    /// Wraps sealed bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 320]) -> Self {
        Self(bytes)
    }

    /// The sealed bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 320] {
        &self.0
    }
}

impl core::fmt::Debug for SealedPushBlob {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("SealedPushBlob(..)")
    }
}

/// SHA-256 of an OHTTP or push relay key config, as pinned in Party state (design B.12).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KeyConfigHash([u8; 32]);

impl KeyConfigHash {
    /// Wraps hash bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The hash bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// An `https://` URL of a relay.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RelayUrl(String);

impl RelayUrl {
    /// Checks the scheme; full URL parsing is the transport's job.
    ///
    /// # Errors
    ///
    /// [`ProtoError::OutOfRange`] unless the URL starts with `https://`.
    pub fn new(url: String) -> Result<Self, ProtoError> {
        if url.starts_with("https://") {
            Ok(Self(url))
        } else {
            Err(ProtoError::OutOfRange { field: "relay url" })
        }
    }

    /// The URL.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Writes the URL as a text string.
    pub(crate) fn encode_cbor(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.text(self.as_str())
    }

    /// Reads and checks a URL.
    pub(crate) fn decode_cbor(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        Self::new(decoder.str()?.to_owned())
    }
}

/// Which push relay can open a sealed push (design D, `RelayRef`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RelayRef {
    /// A relay from the server's advertised list.
    Listed(RelayId),
    /// The device's own relay; accepted only if the operator allows custom relays.
    Custom {
        /// Where the relay is.
        url: RelayUrl,
        /// The pinned hash of its key config.
        key_config_hash: KeyConfigHash,
    },
}

/// Layout: `[0, relay id]` or `[1, url, key config hash]`.
impl CborBody for RelayRef {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        match self {
            Self::Listed(relay_id) => {
                writer.array(2)?;
                writer.unsigned(0_u8);
                writer.unsigned(relay_id.get());
            }
            Self::Custom {
                url,
                key_config_hash,
            } => {
                writer.array(3)?;
                writer.unsigned(1_u8);
                url.encode_cbor(writer)?;
                writer.bytes(key_config_hash.as_bytes())?;
            }
        }
        Ok(())
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        let length = cbor::array(decoder)?;
        match decoder.u8()? {
            0 => {
                cbor::expect_length(length, 2)?;
                Ok(Self::Listed(RelayId::new(decoder.u16()?)))
            }
            1 => {
                cbor::expect_length(length, 3)?;
                Ok(Self::Custom {
                    url: RelayUrl::decode_cbor(decoder)?,
                    key_config_hash: KeyConfigHash::from_bytes(cbor::fixed_bytes(decoder)?),
                })
            }
            _ => Err(ProtoError::OutOfRange { field: "relay ref" }),
        }
    }
}

/// A sealed push token as registered with a mailbox or subscription (design B.4.9).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealedPush {
    /// The relay that can open it.
    pub relay: RelayRef,
    /// The sealed token, fresh for every mailbox and subscription.
    pub blob: SealedPushBlob,
    /// Priority for class Standard; ignored for class Location.
    pub urgency: Urgency,
    /// Returned in the push so the device knows what to read.
    pub handle: PushHandle,
}

/// Layout: `[relay, blob (320 bytes), urgency, handle (8 bytes)]`.
impl CborBody for SealedPush {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(4)?;
        self.relay.encode_body(writer)?;
        writer.bytes(self.blob.as_bytes())?;
        self.urgency.encode_cbor(writer);
        writer.bytes(self.handle.as_bytes())
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 4)?;
        Ok(Self {
            relay: RelayRef::decode_body(decoder)?,
            blob: SealedPushBlob::from_bytes(cbor::fixed_bytes(decoder)?),
            urgency: Urgency::decode_cbor(decoder)?,
            handle: PushHandle::from_bytes(cbor::fixed_bytes(decoder)?),
        })
    }
}

/// `POST /v1/mailboxes` (design D): registers one anonymous mailbox per request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MailboxSpec {
    /// `SHA-256("pingers/v1/mbox" || read_key)[..16]`.
    pub mailbox_id: MailboxId,
    /// Knock and reply mailboxes: one lease. Others: at most 30 days.
    pub time_to_live: TimeToLive,
    /// `None`: nothing can wake the owner for this mailbox.
    pub push: Option<SealedPush>,
}

/// Layout: `[mailbox id, ttl seconds, sealed push or null]`.
impl CborBody for MailboxSpec {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(3)?;
        writer.bytes(self.mailbox_id.as_bytes())?;
        self.time_to_live.encode_cbor(writer)?;
        writer.option(self.push.as_ref(), |writer, push| push.encode_body(writer))
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 3)?;
        Ok(Self {
            mailbox_id: MailboxId::from_bytes(cbor::fixed_bytes(decoder)?),
            time_to_live: TimeToLive::decode_cbor(decoder)?,
            push: cbor::option(decoder, SealedPush::decode_body)?,
        })
    }
}

/// A sealed-push subscription to a group's log (design D). Its `bind` uses the subscription
/// id in place of a mailbox id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Subscription {
    /// Random id, chosen by the subscriber.
    pub id: SubscriptionId,
    /// The sealed token.
    pub push: SealedPush,
    /// How long the server keeps it.
    pub time_to_live: TimeToLive,
}

/// Layout: `[subscription id, sealed push, ttl seconds]`.
impl CborBody for Subscription {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(3)?;
        writer.bytes(self.id.as_bytes())?;
        self.push.encode_body(writer)?;
        self.time_to_live.encode_cbor(writer)
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 3)?;
        Ok(Self {
            id: SubscriptionId::from_bytes(cbor::fixed_bytes(decoder)?),
            push: SealedPush::decode_body(decoder)?,
            time_to_live: TimeToLive::decode_cbor(decoder)?,
        })
    }
}

versioned_cbor!(MailboxSpec, Subscription);

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    #[test]
    fn zero_time_to_live_is_rejected() {
        assert!(TimeToLive::new(Duration::ZERO).is_err());
    }

    #[test]
    fn time_to_live_beyond_u32_seconds_is_rejected() {
        assert!(
            TimeToLive::new(Duration::from_secs(u64::from(u32::MAX).saturating_add(1))).is_err()
        );
    }

    #[test]
    fn time_to_live_of_exactly_u32_max_seconds_is_accepted() {
        assert!(TimeToLive::new(Duration::from_secs(u64::from(u32::MAX))).is_ok());
    }

    #[test]
    fn relay_url_must_be_https() {
        assert!(RelayUrl::new(String::from("http://relay.example")).is_err());
    }

    fn plaintext(token_length: usize) -> PushTokenPlaintext {
        PushTokenPlaintext {
            platform: PushPlatform::UnifiedPush,
            class: PushClass::Location,
            token: PushToken::from_bytes(vec![0xab; token_length]),
            not_after: UnixSeconds::new(0x0102_0304_0506_0708),
        }
    }

    #[test]
    fn push_plaintext_layout_is_header_then_token() -> Result<(), ProtoError> {
        let bytes = plaintext(2).encode_padded()?;
        assert_eq!(
            bytes.get(..15),
            Some(&[1, 2, 1, 1, 2, 3, 4, 5, 6, 7, 8, 0, 2, 0xab, 0xab][..])
        );
        assert!(
            bytes
                .get(15..)
                .is_some_and(|rest| rest.iter().all(|byte| *byte == 0))
        );
        Ok(())
    }

    #[test]
    fn longest_push_token_fills_the_plaintext() -> Result<(), ProtoError> {
        let longest = plaintext(PushTokenPlaintext::MAX_TOKEN_BYTES);
        let bytes = longest.encode_padded()?;
        assert_eq!(bytes.last(), Some(&0xab));
        assert_eq!(PushTokenPlaintext::decode_padded(&bytes)?, longest);
        Ok(())
    }

    #[test]
    fn push_token_one_byte_too_long_is_rejected() {
        let too_long = plaintext(PushTokenPlaintext::MAX_TOKEN_BYTES.saturating_add(1));
        assert!(matches!(
            too_long.encode_padded(),
            Err(ProtoError::OutOfRange {
                field: "push token"
            })
        ));
    }

    #[test]
    fn push_plaintext_with_nonzero_padding_is_rejected() -> Result<(), ProtoError> {
        let mut bytes = plaintext(2).encode_padded()?;
        if let Some(last) = bytes.last_mut() {
            *last = 1;
        }
        assert!(matches!(
            PushTokenPlaintext::decode_padded(&bytes),
            Err(ProtoError::BadPadding)
        ));
        Ok(())
    }

    #[test]
    fn push_token_length_past_the_end_is_rejected() -> Result<(), ProtoError> {
        let mut bytes = plaintext(2).encode_padded()?;
        bytes[11..13].copy_from_slice(&260_u16.to_be_bytes());
        assert!(matches!(
            PushTokenPlaintext::decode_padded(&bytes),
            Err(ProtoError::BadPadding)
        ));
        Ok(())
    }

    #[test]
    fn push_plaintext_header_values_are_checked() -> Result<(), ProtoError> {
        let good = plaintext(2).encode_padded()?;
        let mut version = good;
        version[0] = 2;
        assert!(matches!(
            PushTokenPlaintext::decode_padded(&version),
            Err(ProtoError::UnsupportedVersion { version: 2 })
        ));
        let mut platform = good;
        platform[1] = 0;
        assert!(matches!(
            PushTokenPlaintext::decode_padded(&platform),
            Err(ProtoError::OutOfRange {
                field: "push platform"
            })
        ));
        let mut class = good;
        class[2] = 2;
        assert!(matches!(
            PushTokenPlaintext::decode_padded(&class),
            Err(ProtoError::OutOfRange {
                field: "push class"
            })
        ));
        Ok(())
    }
}
