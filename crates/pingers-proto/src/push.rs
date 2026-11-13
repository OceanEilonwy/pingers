// SPDX-License-Identifier: AGPL-3.0-only
//! Mailbox registration, sealed push tokens and group subscriptions (design B.4.9, D).

use core::time::Duration;

use pingers_time::UnixSeconds;

use crate::stub::not_implemented;
use crate::{MailboxId, ProtoError, PushHandle, RelayId, SubscriptionId, WireDecode, WireEncode};

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
}

/// How urgently a Standard-class push should reach the device (design B.4.9, B.7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Urgency {
    /// Something the user should see: high-priority data message, app posts a notification.
    Alert,
    /// Housekeeping: normal-priority data message; the server may delay it up to 30 s.
    Quiet,
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

/// What the push is for (design B.4.9).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PushClass {
    /// Alerts and housekeeping; [`Urgency`] chooses the priority.
    Standard,
    /// A knock arrived: wake the device for a liveness check (on Android a high-priority FCM
    /// data message). Urgency does not apply.
    Location,
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

    /// The padded plaintext for pingers-crypto to seal.
    ///
    /// # Errors
    ///
    /// [`ProtoError::PayloadTooLarge`] if the token is too long to fit.
    pub fn encode_padded(&self) -> Result<[u8; Self::PADDED_SIZE], ProtoError> {
        not_implemented(self)
    }

    /// Parses an opened plaintext (push relay side).
    ///
    /// # Errors
    ///
    /// [`ProtoError::BadPadding`] or [`ProtoError::OutOfRange`] if the plaintext is malformed.
    pub fn decode_padded(bytes: &[u8; Self::PADDED_SIZE]) -> Result<Self, ProtoError> {
        not_implemented(bytes)
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

impl WireEncode for MailboxSpec {
    fn encode(&self) -> Result<Vec<u8>, ProtoError> {
        not_implemented(self)
    }
}

impl WireDecode for MailboxSpec {
    fn decode(bytes: &[u8]) -> Result<Self, ProtoError> {
        not_implemented(bytes)
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

impl WireEncode for Subscription {
    fn encode(&self) -> Result<Vec<u8>, ProtoError> {
        not_implemented(self)
    }
}

impl WireDecode for Subscription {
    fn decode(bytes: &[u8]) -> Result<Self, ProtoError> {
        not_implemented(bytes)
    }
}

#[cfg(test)]
mod tests {
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
    fn relay_url_must_be_https() {
        assert!(RelayUrl::new(String::from("http://relay.example")).is_err());
    }
}
