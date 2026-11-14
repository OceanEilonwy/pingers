// SPDX-License-Identifier: AGPL-3.0-only
//! Invites (design D, "Invite flow"; join flow A): the payload in an invite link's URL
//! fragment, and what a joiner posts to the invite mailbox: a join request, or the withdrawal
//! of one.
//!
//! The link is `https://<host>/j#v1.<base64url(InvitePayload encoding)>`; the fragment never
//! reaches the server. Posts are sealed by pingers-crypto's `seal_join_request` under
//! `HKDF(invite_secret, "join")`, so opening one proves the poster held the link, and the AEAD
//! authenticates the `KeyPackage` inside under that secret.

use minicbor::Decoder;
use pingers_time::UnixSeconds;

use crate::cbor::{self, CborBody, Writer, versioned_cbor};
use subtle::ConstantTimeEq as _;

use crate::secret::option_ct_eq;
use crate::{
    KeyConfigHash, KeyPackageBytes, MailboxId, ProtoError, SecretBytes, SecretText, ServerId,
    Subscription,
};

/// The first 8 bytes of the inviter's MLS leaf signature key hash: the joiner checks that the
/// group in the Welcome contains a leaf that matches (design D). `Debug` does not print it.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct InviterFingerprint([u8; 8]);

impl InviterFingerprint {
    /// Length in bytes.
    pub const LENGTH: usize = 8;

    /// Wraps fingerprint bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 8]) -> Self {
        Self(bytes)
    }

    /// The fingerprint bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 8] {
        &self.0
    }
}

impl core::fmt::Debug for InviterFingerprint {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("InviterFingerprint(redacted)")
    }
}

/// What an invite link's fragment carries (design D, join flow A): where to post, the secret
/// that authenticates the join request, who to expect in the Welcome, and until when.
///
/// Nothing about the Party (its name, members, colour or suite) is in it, so a link that leaks
/// (forwarded, or read by the app that carried it) reveals only the server. The joiner learns
/// the Party from the Welcome, after the inviter lets them in. Every invite waits for the
/// inviter to let each joiner in and stops working once one has been let in, so the link says
/// nothing about how many may use it. `==` compares the secret in constant time.
#[derive(Clone, Debug)]
pub struct InvitePayload {
    /// The Pingers server the Party lives on; the joiner registers there.
    pub server: ServerId,
    /// The anonymous mailbox the inviter reads join requests from.
    pub invite_mailbox: MailboxId,
    /// The 32-byte invite secret (pingers-crypto's `InviteSecret`); a bearer capability.
    pub invite_secret: SecretBytes<32>,
    /// Checked against the Welcome's leaves.
    pub inviter_fingerprint: InviterFingerprint,
    /// After this the link is refused.
    pub expires: UnixSeconds,
    /// The gateway key config hashes of the Party's server, as the inviter pins them (the key
    /// served now and the next one). A joiner that does not know the server pins these and
    /// refuses a server whose key does not match; nothing is pinned on first use. At least one,
    /// at most [`InvitePayload::MAX_PINS`].
    pub gateway_pins: Vec<KeyConfigHash>,
    /// The key config hashes of the push relay the inviter's server uses, as the inviter pins
    /// them; empty when the inviter has none. At most [`InvitePayload::MAX_PINS`].
    pub push_relay_pins: Vec<KeyConfigHash>,
    /// The operator's registration code (design B.1), passed on by an inviter who registered
    /// with one, so the joiner's first `POST /v1/accounts` on that server needs no typing.
    /// Like the link itself it is a bearer capability. Never empty.
    pub registration_code: Option<SecretText>,
}

impl PartialEq for InvitePayload {
    fn eq(&self, other: &Self) -> bool {
        self.server == other.server
            && self.invite_mailbox == other.invite_mailbox
            && bool::from(self.invite_secret.ct_eq(&other.invite_secret))
            && self.inviter_fingerprint == other.inviter_fingerprint
            && self.expires == other.expires
            && self.gateway_pins == other.gateway_pins
            && self.push_relay_pins == other.push_relay_pins
            && option_ct_eq(
                self.registration_code.as_ref(),
                other.registration_code.as_ref(),
            )
    }
}

impl Eq for InvitePayload {}

impl InvitePayload {
    /// The most pins of either kind a link carries (two-slot rotation needs two).
    pub const MAX_PINS: usize = 4;
}

/// Layout: `[server (text), invite mailbox (16 bytes), invite secret (32 bytes), inviter
/// fingerprint (8 bytes), expires (seconds), gateway pins (array of 32-byte hashes, 1 to 4),
/// push relay pins (array of 32-byte hashes, 0 to 4), registration code (text, only when
/// present)]`. Without a code the array has seven elements.
impl CborBody for InvitePayload {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        let code = registration_code(self.registration_code.as_ref())?;
        check_pins(&self.gateway_pins, 1, "gateway pins")?;
        check_pins(&self.push_relay_pins, 0, "push relay pins")?;
        writer.array(if code.is_some() { 8 } else { 7 })?;
        writer.text(self.server.as_str())?;
        writer.bytes(self.invite_mailbox.as_bytes())?;
        writer.bytes(self.invite_secret.expose())?;
        writer.bytes(self.inviter_fingerprint.as_bytes())?;
        writer.unsigned(self.expires.get());
        for pins in [&self.gateway_pins, &self.push_relay_pins] {
            writer.array(pins.len())?;
            for pin in pins {
                writer.bytes(pin.as_bytes())?;
            }
        }
        if let Some(code) = code {
            writer.text(code)?;
        }
        Ok(())
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        let elements = cbor::array(decoder)?;
        if elements != 8 {
            cbor::expect_length(elements, 7)?;
        }
        Ok(Self {
            server: ServerId::new(decoder.str()?.to_owned())?,
            invite_mailbox: MailboxId::from_bytes(cbor::fixed_bytes(decoder)?),
            invite_secret: SecretBytes::new(cbor::fixed_bytes(decoder)?),
            inviter_fingerprint: InviterFingerprint::from_bytes(cbor::fixed_bytes(decoder)?),
            expires: UnixSeconds::new(decoder.u64()?),
            gateway_pins: decode_pins(decoder, 1, "gateway pins")?,
            push_relay_pins: decode_pins(decoder, 0, "push relay pins")?,
            registration_code: if elements == 8 {
                let code = SecretText::new(decoder.str()?.to_owned());
                registration_code(Some(&code))?;
                Some(code)
            } else {
                None
            },
        })
    }
}

/// Pins within `minimum..=MAX_PINS`.
fn check_pins(
    pins: &[KeyConfigHash],
    minimum: usize,
    field: &'static str,
) -> Result<(), ProtoError> {
    if (minimum..=InvitePayload::MAX_PINS).contains(&pins.len()) {
        Ok(())
    } else {
        Err(ProtoError::OutOfRange { field })
    }
}

fn decode_pins(
    decoder: &mut Decoder<'_>,
    minimum: usize,
    field: &'static str,
) -> Result<Vec<KeyConfigHash>, ProtoError> {
    let count =
        usize::try_from(cbor::array(decoder)?).map_err(|_huge| ProtoError::OutOfRange { field })?;
    if !(minimum..=InvitePayload::MAX_PINS).contains(&count) {
        return Err(ProtoError::OutOfRange { field });
    }
    (0..count)
        .map(|_| Ok(KeyConfigHash::from_bytes(cbor::fixed_bytes(decoder)?)))
        .collect()
}

/// The code's text if there is one; an empty code is refused (it would mean "no code").
fn registration_code(code: Option<&SecretText>) -> Result<Option<&str>, ProtoError> {
    match code.map(SecretText::expose) {
        Some("") => Err(ProtoError::OutOfRange {
            field: "registration code",
        }),
        other => Ok(other),
    }
}

/// A joiner's name as the inviter's "asked to join" screen and the roster show it: 1 to 200
/// bytes of UTF-8, as pingers-store's member names. `Debug` does not print it.
#[derive(Clone, PartialEq, Eq)]
pub struct DisplayName(String);

impl DisplayName {
    /// The longest name in bytes.
    pub const MAX_BYTES: usize = 200;

    /// Checks the length.
    ///
    /// # Errors
    ///
    /// [`ProtoError::OutOfRange`] if it is empty or longer than [`DisplayName::MAX_BYTES`].
    pub fn new(name: String) -> Result<Self, ProtoError> {
        if name.is_empty() || name.len() > Self::MAX_BYTES {
            Err(ProtoError::OutOfRange {
                field: "display name",
            })
        } else {
            Ok(Self(name))
        }
    }

    /// The name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Debug for DisplayName {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("DisplayName(redacted)")
    }
}

/// A join request (design D): the joiner's key package, name, reply mailbox and, if it has
/// wake-up pushes on, its sealed subscription for the Party group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinRequest {
    /// The joiner's MLS `KeyPackage`, which the inviter adds; authenticated by the seal. The
    /// join code both phones show is derived from it (pingers-crypto's `join_code`).
    pub key_package: KeyPackageBytes,
    /// The joiner's X-Wing `KeyPackage` too, for a quantum-resistant Party (the link does not
    /// say which suite the Party uses, so a joiner sends both); `None` on a build that cannot
    /// make one. The inviter adds whichever the Party's suite needs.
    pub quantum_resistant_key_package: Option<KeyPackageBytes>,
    /// Shown to the inviter.
    pub display_name: DisplayName,
    /// Where the inviter posts the Welcome.
    pub reply_mailbox: MailboxId,
    /// The joiner's sealed push subscription for the Party group (`subs_add` of the Commit).
    /// `None` when the joiner has wake-up pushes off: it still joins, and reads the group on
    /// its own wakes instead of being woken.
    pub party_subscription: Option<Subscription>,
}

/// Layout: `[key package, X-Wing key package or null, display name (text), reply mailbox (16
/// bytes), party subscription or null]`.
impl CborBody for JoinRequest {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(5)?;
        writer.bytes(self.key_package.as_bytes())?;
        writer.option(
            self.quantum_resistant_key_package.as_ref(),
            |writer, key_package| writer.bytes(key_package.as_bytes()),
        )?;
        writer.text(self.display_name.as_str())?;
        writer.bytes(self.reply_mailbox.as_bytes())?;
        writer.option(self.party_subscription.as_ref(), |writer, subscription| {
            subscription.encode_body(writer)
        })
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 5)?;
        Ok(Self {
            key_package: KeyPackageBytes::from_bytes(decoder.bytes()?.to_vec()),
            quantum_resistant_key_package: cbor::option(decoder, |decoder| {
                Ok(KeyPackageBytes::from_bytes(decoder.bytes()?.to_vec()))
            })?,
            display_name: DisplayName::new(decoder.str()?.to_owned())?,
            reply_mailbox: MailboxId::from_bytes(cbor::fixed_bytes(decoder)?),
            party_subscription: cbor::option(decoder, Subscription::decode_body)?,
        })
    }
}

/// What a joiner posts to an invite mailbox, sealed with pingers-crypto's `seal_join_request`
/// and padded to a mailbox size class.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InvitePost {
    /// Asks to join (boxed: a join request carries a whole key package).
    Join(Box<JoinRequest>),
    /// Withdraws the request whose Welcome would go to this reply mailbox ("Withdraw
    /// request"). Only the joiner and the inviter know the pairing of request and reply
    /// mailbox; the server also sees the mailbox id, so a server that also held the link could
    /// withdraw a request, which the joiner sees as not being let in.
    Withdraw {
        /// The withdrawn request's reply mailbox.
        reply_mailbox: MailboxId,
    },
}

/// Wire values of [`InvitePost`]'s variants.
mod post_kind {
    pub(super) const JOIN: u8 = 0;
    pub(super) const WITHDRAW: u8 = 1;
}

/// Layout: `[0, join request]` or `[1, reply mailbox (16 bytes)]`.
impl CborBody for InvitePost {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(2)?;
        match self {
            Self::Join(request) => {
                writer.unsigned(post_kind::JOIN);
                request.encode_body(writer)
            }
            Self::Withdraw { reply_mailbox } => {
                writer.unsigned(post_kind::WITHDRAW);
                writer.bytes(reply_mailbox.as_bytes())
            }
        }
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 2)?;
        match decoder.u8()? {
            post_kind::JOIN => Ok(Self::Join(Box::new(JoinRequest::decode_body(decoder)?))),
            post_kind::WITHDRAW => Ok(Self::Withdraw {
                reply_mailbox: MailboxId::from_bytes(cbor::fixed_bytes(decoder)?),
            }),
            _ => Err(ProtoError::OutOfRange {
                field: "invite post kind",
            }),
        }
    }
}

versioned_cbor!(InvitePayload, JoinRequest, InvitePost);

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    #[test]
    fn a_link_carries_one_to_four_gateway_pins_and_at_most_four_push_relay_pins()
    -> Result<(), ProtoError> {
        use crate::WireEncode as _;
        let pins = |count: u8| {
            (0..count)
                .map(|fill| KeyConfigHash::from_bytes([fill; 32]))
                .collect()
        };
        let payload = |gateway: u8, push: u8| -> Result<InvitePayload, ProtoError> {
            Ok(InvitePayload {
                server: ServerId::new("https://pingers.example".to_owned())?,
                invite_mailbox: MailboxId::from_bytes([1; 16]),
                invite_secret: SecretBytes::new([2; 32]),
                inviter_fingerprint: InviterFingerprint::from_bytes([3; 8]),
                expires: UnixSeconds::new(4),
                gateway_pins: pins(gateway),
                push_relay_pins: pins(push),
                registration_code: None,
            })
        };
        assert!(payload(1, 0)?.encode().is_ok());
        assert!(payload(4, 4)?.encode().is_ok());
        assert!(payload(0, 0)?.encode().is_err());
        assert!(payload(5, 0)?.encode().is_err());
        assert!(payload(1, 5)?.encode().is_err());
        Ok(())
    }

    #[test]
    fn display_name_length_is_checked() {
        assert!(DisplayName::new(String::new()).is_err());
        assert!(DisplayName::new("a".repeat(200)).is_ok());
        assert!(DisplayName::new("a".repeat(201)).is_err());
    }

    #[test]
    fn debug_hides_names_and_fingerprints() -> Result<(), ProtoError> {
        let name = DisplayName::new(String::from("Ben"))?;
        assert_eq!(format!("{name:?}"), "DisplayName(redacted)");
        let fingerprint = InviterFingerprint::from_bytes([1; 8]);
        assert_eq!(format!("{fingerprint:?}"), "InviterFingerprint(redacted)");
        Ok(())
    }
}
