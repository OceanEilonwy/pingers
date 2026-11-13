// SPDX-License-Identifier: AGPL-3.0-only
//! The body of `POST /v1/push` (design D, `PushReq`).
//!
//! JSON with hex byte strings:
//!
//! ```json
//! {"key_id":1,"blob":"<640 hex>","bind":"<64 hex>","urgency":"alert","handle":"<16 hex>"}
//! ```
//!
//! `urgency` is `"alert"` or `"quiet"`; the class (Standard or Location) is sealed inside the
//! blob. This wire type belongs in pingers-proto beside `SealedPush`; it lives here until it
//! moves.

use pingers_crypto::PushBind;
use pingers_proto::{PushHandle, SealedPushBlob, Urgency};
use serde::{Deserialize, Serialize};

use crate::keys::KeyId;

/// Why a push request body was refused. Names the field, never its value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RequestError {
    /// Not JSON, or a field is missing or of the wrong type.
    #[error("malformed push request")]
    Malformed,
    /// A byte field is not hex of the required length.
    #[error("{field} must be {bytes} bytes of hex")]
    BadField {
        /// The field.
        field: &'static str,
        /// The required length.
        bytes: usize,
    },
}

/// A parsed push request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushRequest {
    /// Which relay key the blob is sealed to.
    pub key_id: KeyId,
    /// The sealed token.
    pub blob: SealedPushBlob,
    /// `SHA-256("pingers/v1/push-bind" || mailbox_id)`; the relay never sees the id.
    pub bind: PushBind,
    /// Priority for class Standard.
    pub urgency: Urgency,
    /// Echoed in the push so the device knows what to read.
    pub handle: PushHandle,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum WireUrgency {
    Alert,
    Quiet,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WireRequest {
    key_id: u8,
    blob: String,
    bind: String,
    urgency: WireUrgency,
    handle: String,
}

fn decode_hex<const N: usize>(field: &'static str, text: &str) -> Result<[u8; N], RequestError> {
    let mut bytes = [0; N];
    hex::decode_to_slice(text, &mut bytes)
        .map_err(|_| RequestError::BadField { field, bytes: N })?;
    Ok(bytes)
}

impl PushRequest {
    /// Parses a request body.
    ///
    /// # Errors
    ///
    /// [`RequestError::Malformed`] if the JSON does not match the schema;
    /// [`RequestError::BadField`] if a byte field has the wrong length or is not hex.
    pub fn decode_json(body: &[u8]) -> Result<Self, RequestError> {
        let wire: WireRequest =
            serde_json::from_slice(body).map_err(|_| RequestError::Malformed)?;
        Ok(Self {
            key_id: KeyId::new(wire.key_id),
            blob: SealedPushBlob::from_bytes(decode_hex("blob", &wire.blob)?),
            bind: PushBind::from_bytes(decode_hex("bind", &wire.bind)?),
            urgency: match wire.urgency {
                WireUrgency::Alert => Urgency::Alert,
                WireUrgency::Quiet => Urgency::Quiet,
            },
            handle: PushHandle::from_bytes(decode_hex("handle", &wire.handle)?),
        })
    }

    /// The request body, as a Pingers server sends it.
    #[must_use]
    pub fn encode_json(&self) -> Vec<u8> {
        let wire = WireRequest {
            key_id: self.key_id.get(),
            blob: hex::encode(self.blob.as_bytes()),
            bind: hex::encode(self.bind.as_bytes()),
            urgency: match self.urgency {
                Urgency::Alert => WireUrgency::Alert,
                Urgency::Quiet => WireUrgency::Quiet,
            },
            handle: hex::encode(self.handle.as_bytes()),
        };
        // Serialising a struct of strings and integers to a Vec cannot fail.
        serde_json::to_vec(&wire).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    fn sample(urgency: Urgency) -> PushRequest {
        PushRequest {
            key_id: KeyId::new(3),
            blob: SealedPushBlob::from_bytes([0xab; 320]),
            bind: PushBind::from_bytes([0x11; 32]),
            urgency,
            handle: PushHandle::from_bytes([1, 2, 3, 4, 5, 6, 7, 8]),
        }
    }

    #[test]
    fn decode_of_encode_is_identity() -> Result<(), RequestError> {
        for urgency in [Urgency::Alert, Urgency::Quiet] {
            let request = sample(urgency);
            assert_eq!(PushRequest::decode_json(&request.encode_json())?, request);
        }
        Ok(())
    }

    #[test]
    fn the_wire_format_is_lowercase_hex_and_lowercase_urgency() {
        let body = String::from_utf8(sample(Urgency::Quiet).encode_json()).unwrap_or_default();
        assert!(body.contains(r#""urgency":"quiet""#));
        assert!(body.contains(r#""handle":"0102030405060708""#));
        assert!(body.contains(r#""key_id":3"#));
    }

    fn body_with(field: &str, value: &str) -> Vec<u8> {
        let mut wire: serde_json::Value =
            serde_json::from_slice(&sample(Urgency::Alert).encode_json()).unwrap_or_default();
        if let Some(object) = wire.as_object_mut() {
            object.insert(
                field.to_owned(),
                serde_json::Value::String(value.to_owned()),
            );
        }
        serde_json::to_vec(&wire).unwrap_or_default()
    }

    #[test]
    fn a_short_bind_is_refused() {
        assert_eq!(
            PushRequest::decode_json(&body_with("bind", &"11".repeat(31))),
            Err(RequestError::BadField {
                field: "bind",
                bytes: 32
            })
        );
    }

    #[test]
    fn a_long_handle_is_refused() {
        assert_eq!(
            PushRequest::decode_json(&body_with("handle", &"01".repeat(9))),
            Err(RequestError::BadField {
                field: "handle",
                bytes: 8
            })
        );
    }

    #[test]
    fn a_non_hex_blob_is_refused() {
        assert_eq!(
            PushRequest::decode_json(&body_with("blob", &"zz".repeat(320))),
            Err(RequestError::BadField {
                field: "blob",
                bytes: 320
            })
        );
    }

    #[test]
    fn an_unknown_urgency_or_extra_field_is_malformed() {
        assert_eq!(
            PushRequest::decode_json(&body_with("urgency", "location")),
            Err(RequestError::Malformed)
        );
        assert_eq!(
            PushRequest::decode_json(&body_with("mailbox_id", "00")),
            Err(RequestError::Malformed)
        );
        assert_eq!(
            PushRequest::decode_json(b"not json"),
            Err(RequestError::Malformed)
        );
    }
}
