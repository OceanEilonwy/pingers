// SPDX-License-Identifier: AGPL-3.0-only
//! The first-phone setup link: what the owner's appliance shows as a QR code so their first
//! phone registers on a server that requires a registration code (design B.1), without typing
//! the code and without a debug build.
//!
//! The link is `https://<server>/j#s1.<base64url(SetupPayload encoding)>`, in the same family
//! as invite links (`/j#v1.`): the server's landing page at `/j` and the app's App Links handle
//! both, and the fragment never reaches the server. The app also accepts the
//! `pingers://join#s1.<...>` form the landing page forwards.
//!
//! Besides the server and the code, the payload carries the key pins the appliance computed
//! from its own keys (the OHTTP gateway's and the push relay's, the served key first), the same
//! values the owner copies into the app build's `PINGERS_GATEWAY_KEY_PINS` and
//! `PINGERS_PUSH_RELAY_KEY_PINS`. The app checks them against its build's pins, or, in a build
//! without pins, against the keys it meets before it registers.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use minicbor::Decoder;
use subtle::ConstantTimeEq as _;
use zeroize::Zeroizing;

use crate::cbor::{self, CborBody, Writer, versioned_cbor};
use crate::{KeyConfigHash, ProtoError, SecretText, ServerId, WireDecode as _, WireEncode as _};

/// The part of an `https://` setup link between the server origin and the payload.
const PATH_AND_VERSION: &str = "/j#s1.";

/// The app's own scheme, as the landing page forwards a link to it.
const APP_PREFIX: &str = "pingers://join#s1.";

/// The fragment's version prefix, which tells a setup link from an invite (`v1.`).
pub const SETUP_FRAGMENT_PREFIX: &str = "s1.";

/// The most pins of one kind a link carries: two slots, with room for an overlap.
pub const MAX_SETUP_PINS: usize = 4;

/// The longest registration code a link carries, as servers accept them.
pub const MAX_REGISTRATION_CODE_BYTES: usize = 128;

/// What a setup link's fragment carries. `==` compares the code in constant time; `Debug`
/// prints no code.
#[derive(Clone, Debug)]
pub struct SetupPayload {
    /// The server origin; the app registers there.
    pub server: ServerId,
    /// The operator's registration code, a bearer capability like the link itself.
    pub registration_code: SecretText,
    /// The OHTTP gateway's key pins, the served key first. One to [`MAX_SETUP_PINS`].
    pub gateway_pins: Vec<KeyConfigHash>,
    /// The push relay's key pins, the published key first. One to [`MAX_SETUP_PINS`].
    pub push_relay_pins: Vec<KeyConfigHash>,
}

impl PartialEq for SetupPayload {
    fn eq(&self, other: &Self) -> bool {
        self.server == other.server
            && bool::from(self.registration_code.ct_eq(&other.registration_code))
            && self.gateway_pins == other.gateway_pins
            && self.push_relay_pins == other.push_relay_pins
    }
}

impl Eq for SetupPayload {}

impl SetupPayload {
    /// The link for this payload: the server origin, `/j#s1.` and the base64url (unpadded) of
    /// the encoding. Zeroed on drop, since it carries the code.
    ///
    /// # Errors
    ///
    /// [`ProtoError::OutOfRange`] if a field is out of range (see [`SetupPayload::check`]).
    pub fn to_link(&self) -> Result<Zeroizing<String>, ProtoError> {
        let encoded = Zeroizing::new(self.encode()?);
        let mut link = Zeroizing::new(String::new());
        link.push_str(self.server.as_str());
        link.push_str(PATH_AND_VERSION);
        URL_SAFE_NO_PAD.encode_string(&*encoded, &mut link);
        Ok(link)
    }

    /// Whether `text` looks like a setup link (either form), without decoding it: how the app
    /// tells a setup link from an invite link before choosing a screen.
    #[must_use]
    pub fn is_setup_link(text: &str) -> bool {
        text.starts_with(APP_PREFIX)
            || text
                .split_once(PATH_AND_VERSION)
                .is_some_and(|(origin, _)| origin.starts_with("https://") && !origin.contains('#'))
    }

    /// Decodes a setup link: `https://<server>/j#s1.<payload>`, whose origin must be the server
    /// inside, or `pingers://join#s1.<payload>`.
    ///
    /// # Errors
    ///
    /// [`ProtoError::OutOfRange`] with field "setup link" if the text is neither form, is not
    /// base64url, or its origin differs from the server inside; any decoding error of the
    /// payload itself.
    pub fn from_link(text: &str) -> Result<Self, ProtoError> {
        let bad = || ProtoError::OutOfRange {
            field: "setup link",
        };
        let (origin, fragment) = if let Some(fragment) = text.strip_prefix(APP_PREFIX) {
            (None, fragment)
        } else {
            let (origin, fragment) = text.split_once(PATH_AND_VERSION).ok_or_else(bad)?;
            (Some(origin), fragment)
        };
        let bytes = Zeroizing::new(URL_SAFE_NO_PAD.decode(fragment).map_err(|_| bad())?);
        let payload = Self::decode(&bytes)?;
        match origin {
            Some(origin) if origin != payload.server.as_str() => Err(bad()),
            _ => Ok(payload),
        }
    }

    /// Checks the code and the pin counts.
    ///
    /// # Errors
    ///
    /// [`ProtoError::OutOfRange`] for an empty or over-long code, or no or too many pins.
    pub fn check(&self) -> Result<(), ProtoError> {
        let code = self.registration_code.expose();
        if code.is_empty() || code.len() > MAX_REGISTRATION_CODE_BYTES {
            return Err(ProtoError::OutOfRange {
                field: "registration code",
            });
        }
        for (pins, field) in [
            (&self.gateway_pins, "gateway pins"),
            (&self.push_relay_pins, "push relay pins"),
        ] {
            if pins.is_empty() || pins.len() > MAX_SETUP_PINS {
                return Err(ProtoError::OutOfRange { field });
            }
        }
        Ok(())
    }
}

fn write_pins(writer: &mut Writer, pins: &[KeyConfigHash]) -> Result<(), ProtoError> {
    writer.array(pins.len())?;
    for pin in pins {
        writer.bytes(pin.as_bytes())?;
    }
    Ok(())
}

fn read_pins(decoder: &mut Decoder<'_>) -> Result<Vec<KeyConfigHash>, ProtoError> {
    let count = cbor::array(decoder)?;
    let count = usize::try_from(count).map_err(|_| ProtoError::OutOfRange { field: "pins" })?;
    if count > MAX_SETUP_PINS {
        return Err(ProtoError::OutOfRange { field: "pins" });
    }
    let mut pins = Vec::with_capacity(count);
    for _ in 0..count {
        pins.push(KeyConfigHash::from_bytes(cbor::fixed_bytes(decoder)?));
    }
    Ok(pins)
}

/// Layout: `[server (text), registration code (text), gateway pins ([32 bytes]), push relay
/// pins ([32 bytes])]`.
impl CborBody for SetupPayload {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        self.check()?;
        writer.array(4)?;
        writer.text(self.server.as_str())?;
        writer.text(self.registration_code.expose())?;
        write_pins(writer, &self.gateway_pins)?;
        write_pins(writer, &self.push_relay_pins)
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 4)?;
        let payload = Self {
            server: ServerId::new(decoder.str()?.to_owned())?,
            registration_code: SecretText::new(decoder.str()?.to_owned()),
            gateway_pins: read_pins(decoder)?,
            push_relay_pins: read_pins(decoder)?,
        };
        payload.check()?;
        Ok(payload)
    }
}

versioned_cbor!(SetupPayload);

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    fn payload() -> Result<SetupPayload, ProtoError> {
        Ok(SetupPayload {
            server: ServerId::new("https://pingers.example".into())?,
            registration_code: SecretText::new("0123456789abcdef0123456789abcdef".into()),
            gateway_pins: vec![
                KeyConfigHash::from_bytes([1; 32]),
                KeyConfigHash::from_bytes([2; 32]),
            ],
            push_relay_pins: vec![
                KeyConfigHash::from_bytes([3; 32]),
                KeyConfigHash::from_bytes([4; 32]),
            ],
        })
    }

    #[test]
    fn a_link_round_trips_in_both_forms() -> Result<(), ProtoError> {
        let payload = payload()?;
        let link = payload.to_link()?;
        assert!(link.starts_with("https://pingers.example/j#s1."));
        assert!(SetupPayload::is_setup_link(&link));
        assert_eq!(SetupPayload::from_link(&link)?, payload);
        let fragment = link
            .split_once('#')
            .map(|(_, fragment)| fragment)
            .ok_or(ProtoError::TrailingBytes)?;
        let forwarded = format!("pingers://join#{fragment}");
        assert!(SetupPayload::is_setup_link(&forwarded));
        assert_eq!(SetupPayload::from_link(&forwarded)?, payload);
        Ok(())
    }

    #[test]
    fn a_link_fits_a_small_qr_code() -> Result<(), ProtoError> {
        // A long server name and two pins of each kind stay well under 400 characters, which
        // a version 13 QR code holds at error correction level M.
        let mut payload = payload()?;
        payload.server = ServerId::new(format!("https://{}.example.org", "p".repeat(40)))?;
        assert!(payload.to_link()?.len() < 400);
        Ok(())
    }

    #[test]
    fn a_link_moved_to_another_origin_is_refused() -> Result<(), ProtoError> {
        let link = payload()?.to_link()?;
        let moved = link.replacen("https://pingers.example", "https://evil.example", 1);
        assert!(SetupPayload::from_link(&moved).is_err());
        Ok(())
    }

    #[test]
    fn invite_links_and_junk_are_not_setup_links() {
        assert!(!SetupPayload::is_setup_link("https://pingers.example/j#v1.AAAA"));
        assert!(!SetupPayload::is_setup_link("pingers://join#v1.AAAA"));
        assert!(!SetupPayload::is_setup_link("http://pingers.example/j#s1.AAAA"));
        assert!(SetupPayload::from_link("https://pingers.example/j#s1.!!").is_err());
        assert!(SetupPayload::from_link("pingers://join#s1.").is_err());
    }

    #[test]
    fn codes_and_pin_counts_are_checked_both_ways() -> Result<(), ProtoError> {
        let mut empty_code = payload()?;
        empty_code.registration_code = SecretText::new(String::new());
        assert!(empty_code.encode().is_err());
        let mut long_code = payload()?;
        long_code.registration_code = SecretText::new("a".repeat(129));
        assert!(long_code.encode().is_err());
        let mut no_pins = payload()?;
        no_pins.gateway_pins.clear();
        assert!(no_pins.encode().is_err());
        let mut many_pins = payload()?;
        many_pins.push_relay_pins = vec![KeyConfigHash::from_bytes([9; 32]); 5];
        assert!(many_pins.encode().is_err());
        Ok(())
    }

    #[test]
    fn equality_covers_every_field_and_debug_hides_the_code() -> Result<(), ProtoError> {
        let one = payload()?;
        let mut other = payload()?;
        other.registration_code = SecretText::new("another-code-0123456789".into());
        assert_ne!(one, other);
        let mut pins = payload()?;
        pins.push_relay_pins.reverse();
        assert_ne!(one, pins);
        assert!(!format!("{one:?}").contains("0123456789abcdef"));
        Ok(())
    }
}
