// SPDX-License-Identifier: AGPL-3.0-only
//! Invite links: `https://<host>/j#v1.<base64url(InvitePayload)>` (design D, invite flow).

use core::fmt;

use pingers_proto::{InvitePayload, WireDecode as _, WireEncode as _};
use zeroize::Zeroizing;

use crate::PartiesError;

const PATH_AND_VERSION: &str = "/j#v1.";

/// An invite link or the string in its QR code. It carries the invite secret in its fragment,
/// so it is zeroed on drop and its `Debug` prints nothing of it.
pub struct InviteLink(Zeroizing<String>);

impl InviteLink {
    /// The link for a payload: the payload's server origin, then `/j#v1.` and the base64url
    /// (unpadded) of its encoding. The fragment never reaches the server.
    ///
    /// # Errors
    ///
    /// [`PartiesError::Proto`] if the payload does not encode.
    pub fn new(payload: &InvitePayload) -> Result<Self, PartiesError> {
        let encoded = Zeroizing::new(payload.encode()?);
        let mut link = Zeroizing::new(String::new());
        link.push_str(payload.server.as_str());
        link.push_str(PATH_AND_VERSION);
        base64url_encode(&encoded, &mut link);
        Ok(Self(link))
    }

    /// Wraps a link the user pasted or scanned; [`Self::payload`] checks it.
    #[must_use]
    pub fn from_text(text: String) -> Self {
        Self(Zeroizing::new(text))
    }

    /// The link text, for sharing or a QR code.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Decodes the link's payload, checking that the link's origin is the payload's server.
    ///
    /// # Errors
    ///
    /// [`PartiesError::InvalidLink`] if the link is not `<server>/j#v1.<base64url>` or its
    /// origin differs from the server inside; [`PartiesError::Proto`] if the payload does not
    /// decode.
    pub fn payload(&self) -> Result<InvitePayload, PartiesError> {
        let (origin, fragment) = self
            .0
            .split_once(PATH_AND_VERSION)
            .ok_or(PartiesError::InvalidLink)?;
        let bytes = Zeroizing::new(base64url_decode(fragment).ok_or(PartiesError::InvalidLink)?);
        let payload = InvitePayload::decode(&bytes)?;
        if payload.server.as_str() == origin {
            Ok(payload)
        } else {
            Err(PartiesError::InvalidLink)
        }
    }
}

impl fmt::Debug for InviteLink {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("InviteLink(redacted)")
    }
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// RFC 4648 §5 base64url without padding. A generic helper (no domain meaning); no workspace
/// dependency provides it, and it is twenty lines.
fn base64url_encode(bytes: &[u8], out: &mut String) {
    for chunk in bytes.chunks(3) {
        let mut group = [0_u8; 3];
        for (target, source) in group.iter_mut().zip(chunk) {
            *target = *source;
        }
        let [first, second, third] = group;
        let indices = [
            first >> 2,
            ((first & 0b11) << 4) | (second >> 4),
            ((second & 0b1111) << 2) | (third >> 6),
            third & 0b11_1111,
        ];
        // One input byte gives two characters, two give three, three give four.
        let characters = chunk.len().saturating_add(1);
        for index in indices.iter().take(characters) {
            if let Some(character) = ALPHABET.get(usize::from(*index)) {
                out.push(char::from(*character));
            }
        }
    }
}

fn base64url_value(character: u8) -> Option<u8> {
    ALPHABET
        .iter()
        .position(|candidate| *candidate == character)
        .and_then(|position| u8::try_from(position).ok())
}

fn base64url_decode(text: &str) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    for chunk in text.as_bytes().chunks(4) {
        let mut values = [0_u8; 4];
        for (target, source) in values.iter_mut().zip(chunk) {
            *target = base64url_value(*source)?;
        }
        let [first, second, third, fourth] = values;
        let decoded = [
            (first << 2) | (second >> 4),
            (second << 4) | (third >> 2),
            (third << 6) | fourth,
        ];
        let produced = match chunk.len() {
            2 => 1,
            3 => 2,
            4 => 3,
            _ => return None,
        };
        // Unused low bits of the last character must be zero (canonical encoding).
        let leftover = match produced {
            1 => second & 0b1111,
            2 => third & 0b11,
            _ => 0,
        };
        if leftover != 0 {
            return None;
        }
        bytes.extend(decoded.iter().take(produced));
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_proto::{InviteUses, InviterFingerprint, MailboxId, SecretBytes, ServerId};
    use pingers_time::UnixSeconds;

    use super::*;
    use crate::test_support::TestError;

    #[test]
    fn base64url_matches_rfc_4648_vectors() {
        // RFC 4648 §10, without padding, plus bytes that use `-` and `_`.
        let vectors: [(&[u8], &str); 8] = [
            (b"", ""),
            (b"f", "Zg"),
            (b"fo", "Zm8"),
            (b"foo", "Zm9v"),
            (b"foob", "Zm9vYg"),
            (b"fooba", "Zm9vYmE"),
            (b"foobar", "Zm9vYmFy"),
            (&[0xfb, 0xff, 0xbf], "-_-_"),
        ];
        for (bytes, text) in vectors {
            let mut encoded = String::new();
            base64url_encode(bytes, &mut encoded);
            assert_eq!(encoded, text);
            assert_eq!(base64url_decode(text).as_deref(), Some(bytes));
        }
    }

    #[test]
    fn base64url_refuses_bad_lengths_characters_and_leftover_bits() {
        assert_eq!(base64url_decode("Z"), None);
        assert_eq!(base64url_decode("Zm9v="), None);
        assert_eq!(base64url_decode("Zh"), None);
        assert_eq!(base64url_decode("Zm9"), None);
        assert_eq!(base64url_decode("Zm-"), None);
    }

    fn payload() -> Result<InvitePayload, TestError> {
        Ok(InvitePayload {
            server: ServerId::new("https://pingers.example".into())?,
            invite_mailbox: MailboxId::from_bytes([3; 16]),
            invite_secret: SecretBytes::new([4; 32]),
            inviter_fingerprint: InviterFingerprint::from_bytes([5; 8]),
            expires: UnixSeconds::new(1_000_000),
            uses: InviteUses::Single,
            gateway_pins: vec![pingers_proto::KeyConfigHash::from_bytes([6; 32])],
            push_relay_pins: Vec::new(),
            registration_code: None,
        })
    }

    #[test]
    fn links_round_trip_and_name_the_server() -> Result<(), TestError> {
        let link = InviteLink::new(&payload()?)?;
        assert!(link.as_str().starts_with("https://pingers.example/j#v1."));
        assert_eq!(link.payload()?, payload()?);
        assert_eq!(format!("{link:?}"), "InviteLink(redacted)");
        Ok(())
    }

    #[test]
    fn links_for_another_origin_or_format_are_invalid() -> Result<(), TestError> {
        let link = InviteLink::new(&payload()?)?;
        let moved = link.as_str().replace("pingers.example", "evil.example");
        assert!(matches!(
            InviteLink::from_text(moved).payload(),
            Err(PartiesError::InvalidLink)
        ));
        let other_version = link.as_str().replace("#v1.", "#v2.");
        assert!(matches!(
            InviteLink::from_text(other_version).payload(),
            Err(PartiesError::InvalidLink)
        ));
        let bad_characters = format!("{}!", link.as_str());
        assert!(matches!(
            InviteLink::from_text(bad_characters).payload(),
            Err(PartiesError::InvalidLink)
        ));
        Ok(())
    }
}
