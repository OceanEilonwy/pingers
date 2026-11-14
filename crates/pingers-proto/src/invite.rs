// SPDX-License-Identifier: AGPL-3.0-only
//! Invites (design D, "Invite flow"): the payload in an invite link's URL fragment, and the
//! join request a joiner posts to the invite mailbox.
//!
//! The link is `https://<host>/j#v1.<base64url(InvitePayload encoding)>`; the fragment never
//! reaches the server. The join request is sealed by pingers-crypto's `seal_join_request` under
//! `HKDF(invite_secret, "join")`, so opening it proves the joiner held the link, and the AEAD
//! authenticates the `KeyPackage` inside under that secret.

use minicbor::Decoder;
use pingers_time::UnixSeconds;

use crate::cbor::{self, CborBody, Writer, versioned_cbor};
use subtle::ConstantTimeEq as _;

use crate::secret::option_ct_eq;
use crate::{
    KeyPackageBytes, MailboxId, ProtoError, SecretBytes, SecretText, ServerId, Subscription,
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

/// How many people a link admits (design D, `single_use`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InviteUses {
    /// Any number of joiners until it expires; the inviter approves each ("Bob wants to join").
    Multiple,
    /// One joiner, approved automatically; the invite mailbox is deleted after.
    Single,
}

impl InviteUses {
    /// Writes the wire value: 0 for Multiple, 1 for Single.
    fn encode_cbor(self, writer: &mut Writer) {
        writer.unsigned(match self {
            Self::Multiple => 0_u8,
            Self::Single => 1,
        });
    }

    fn decode_cbor(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        match decoder.u8()? {
            0 => Ok(Self::Multiple),
            1 => Ok(Self::Single),
            _ => Err(ProtoError::OutOfRange {
                field: "invite uses",
            }),
        }
    }
}

/// What an invite link's fragment carries (design D): where to post, the secret that
/// authenticates the join request, who to expect in the Welcome, and until when.
/// `==` compares the secret in constant time.
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
    /// Single or multiple use.
    pub uses: InviteUses,
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
            && self.uses == other.uses
            && option_ct_eq(
                self.registration_code.as_ref(),
                other.registration_code.as_ref(),
            )
    }
}

impl Eq for InvitePayload {}

/// Layout: `[server (text), invite mailbox (16 bytes), invite secret (32 bytes), inviter
/// fingerprint (8 bytes), expires (seconds), uses (0 multiple, 1 single), registration code
/// (text, only when present)]`. Without a code the array has six elements, as before the field
/// existed, so older links still decode and links without a code are unchanged.
impl CborBody for InvitePayload {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        let code = registration_code(self.registration_code.as_ref())?;
        writer.array(if code.is_some() { 7 } else { 6 })?;
        writer.text(self.server.as_str())?;
        writer.bytes(self.invite_mailbox.as_bytes())?;
        writer.bytes(self.invite_secret.expose())?;
        writer.bytes(self.inviter_fingerprint.as_bytes())?;
        writer.unsigned(self.expires.get());
        self.uses.encode_cbor(writer);
        if let Some(code) = code {
            writer.text(code)?;
        }
        Ok(())
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        let elements = cbor::array(decoder)?;
        if elements != 7 {
            cbor::expect_length(elements, 6)?;
        }
        Ok(Self {
            server: ServerId::new(decoder.str()?.to_owned())?,
            invite_mailbox: MailboxId::from_bytes(cbor::fixed_bytes(decoder)?),
            invite_secret: SecretBytes::new(cbor::fixed_bytes(decoder)?),
            inviter_fingerprint: InviterFingerprint::from_bytes(cbor::fixed_bytes(decoder)?),
            expires: UnixSeconds::new(decoder.u64()?),
            uses: InviteUses::decode_cbor(decoder)?,
            registration_code: if elements == 7 {
                let code = SecretText::new(decoder.str()?.to_owned());
                registration_code(Some(&code))?;
                Some(code)
            } else {
                None
            },
        })
    }
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

/// A joiner's name as the inviter's "wants to join" prompt and the roster show it: 1 to 200
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

/// The plaintext of a join request (design D): sealed with pingers-crypto's
/// `seal_join_request` and posted, padded, to the invite mailbox.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinRequest {
    /// The joiner's MLS `KeyPackage`, which the inviter adds; authenticated by the seal.
    pub key_package: KeyPackageBytes,
    /// Shown to the inviter.
    pub display_name: DisplayName,
    /// Where the inviter posts the Welcome.
    pub reply_mailbox: MailboxId,
    /// The joiner's sealed push subscription for the Party group (`subs_add` of the Commit).
    pub party_subscription: Subscription,
}

/// Layout: `[key package, display name (text), reply mailbox (16 bytes), party subscription]`.
impl CborBody for JoinRequest {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(4)?;
        writer.bytes(self.key_package.as_bytes())?;
        writer.text(self.display_name.as_str())?;
        writer.bytes(self.reply_mailbox.as_bytes())?;
        self.party_subscription.encode_body(writer)
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 4)?;
        Ok(Self {
            key_package: KeyPackageBytes::from_bytes(decoder.bytes()?.to_vec()),
            display_name: DisplayName::new(decoder.str()?.to_owned())?,
            reply_mailbox: MailboxId::from_bytes(cbor::fixed_bytes(decoder)?),
            party_subscription: Subscription::decode_body(decoder)?,
        })
    }
}

versioned_cbor!(InvitePayload, JoinRequest);

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

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
