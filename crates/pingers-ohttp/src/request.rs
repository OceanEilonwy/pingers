// SPDX-License-Identifier: AGPL-3.0-only
//! One request, one response (RFC 9458).

use rand_core::CryptoRng;

use crate::config::{
    AEAD_AES_128_GCM, KDF_HKDF_SHA256, KEM_X25519, REQUEST_HEADER_LENGTH, X25519_KEY_LENGTH,
};
use crate::{BinaryRequest, BinaryResponse, GatewayKey, KeyConfig, OhttpError};

/// The length of the header plus the encapsulated key that start every request.
pub(crate) const ENCAPSULATION_LENGTH: usize = REQUEST_HEADER_LENGTH + X25519_KEY_LENGTH;

/// The lengths a one-shot request's Binary HTTP encoding is padded to (RFC 9292 §3.8),
/// smallest first ([`encapsulate_request_classed`]): its size then says only which class it is
/// in, not its path, headers or credential. The largest, with the 55 bytes of encapsulation,
/// is the gateway's 64 KiB request limit.
pub const REQUEST_CLASSES: [usize; 4] = [2_048, 8_192, 32_768, 65_472];

/// The lengths a one-shot response's encoding is padded to, smallest first
/// ([`Responder::seal_classed`]). 320 KiB holds a full board page (design B.4.3); the largest,
/// 1 MiB and 8 KiB, holds the largest map host answer the server relays (1 MiB and 4 KiB).
pub const RESPONSE_CLASSES: [usize; 5] = [4_096, 16_384, 65_536, 327_680, 1_056_768];

/// The longest sealed response: the largest class with its 16-byte response nonce and 16-byte
/// tag. A client reading one-shot responses accepts this much.
pub const MAX_SEALED_RESPONSE: usize = 1_056_768 + 32;

/// The smallest of `classes` that holds `length` bytes.
fn class_for(length: usize, classes: &[usize]) -> Result<usize, OhttpError> {
    classes
        .iter()
        .copied()
        .find(|class| *class >= length)
        .ok_or(OhttpError::LongerThanPadding)
}

/// An encapsulated request ready to POST to the relay as `message/ohttp-req`, and the context
/// that opens its one response.
pub struct EncapsulatedRequest {
    /// The request body to send.
    pub message: Vec<u8>,
    /// Opens the response; consumed by use, so it cannot open two.
    pub response_context: ResponseContext,
}

/// The client's HPKE context for one response. A fresh one per request keeps anonymous
/// requests unlinkable (design B.12).
pub struct ResponseContext {
    inner: ohttp::ClientResponse,
}

impl ResponseContext {
    /// Opens the `message/ohttp-res` body.
    ///
    /// # Errors
    ///
    /// [`OhttpError::OpenFailed`] if it does not decrypt; [`OhttpError::BadMessage`] if the
    /// inner response is malformed.
    pub fn open(self, response: &[u8]) -> Result<BinaryResponse, OhttpError> {
        let plaintext = self
            .inner
            .decapsulate(response)
            .map_err(|_| OhttpError::OpenFailed)?;
        BinaryResponse::decode(&plaintext)
    }
}

/// Encapsulates `request` to the gateway key in `config`, with fresh HPKE randomness.
///
/// # Errors
///
/// [`OhttpError::BadMessage`] if the request cannot be encoded; [`OhttpError::Internal`] if
/// HPKE fails.
pub fn encapsulate_request(
    config: &KeyConfig,
    request: &BinaryRequest,
    rng: &mut impl CryptoRng,
) -> Result<EncapsulatedRequest, OhttpError> {
    seal_request(config, &request.encode()?, rng)
}

/// As [`encapsulate_request`], with the encoded request padded with zeros to exactly
/// `padded_length` bytes (RFC 9292 §3.8), so its size says nothing about the path or headers.
///
/// # Errors
///
/// [`OhttpError::LongerThanPadding`] if the encoded request is longer than `padded_length`;
/// otherwise as [`encapsulate_request`].
pub fn encapsulate_request_padded(
    config: &KeyConfig,
    request: &BinaryRequest,
    padded_length: usize,
    rng: &mut impl CryptoRng,
) -> Result<EncapsulatedRequest, OhttpError> {
    seal_request(config, &padded(request.encode()?, padded_length)?, rng)
}

/// As [`encapsulate_request`], with the encoded request padded with zeros to the smallest of
/// [`REQUEST_CLASSES`] that holds it: every request in one class has the same length.
///
/// # Errors
///
/// [`OhttpError::LongerThanPadding`] if the encoded request is longer than the largest class;
/// otherwise as [`encapsulate_request`].
pub fn encapsulate_request_classed(
    config: &KeyConfig,
    request: &BinaryRequest,
    rng: &mut impl CryptoRng,
) -> Result<EncapsulatedRequest, OhttpError> {
    let encoded = request.encode()?;
    let length = class_for(encoded.len(), &REQUEST_CLASSES)?;
    seal_request(config, &padded(encoded, length)?, rng)
}

fn seal_request(
    config: &KeyConfig,
    encoded: &[u8],
    rng: &mut impl CryptoRng,
) -> Result<EncapsulatedRequest, OhttpError> {
    let (message, inner) = config
        .client()?
        .encapsulate(encoded, rng)
        .map_err(|_| OhttpError::Internal)?;
    Ok(EncapsulatedRequest {
        message,
        response_context: ResponseContext { inner },
    })
}

/// `encoded` with zeros appended up to `padded_length`.
fn padded(mut encoded: Vec<u8>, padded_length: usize) -> Result<Vec<u8>, OhttpError> {
    if encoded.len() > padded_length {
        return Err(OhttpError::LongerThanPadding);
    }
    encoded.resize(padded_length, 0);
    Ok(encoded)
}

/// The gateway side: holds the current (and, during rotation, previous) key.
pub struct Gateway {
    keys: Vec<GatewayKey>,
}

impl Gateway {
    /// A gateway accepting requests to any of `keys`.
    #[must_use]
    pub const fn new(keys: Vec<GatewayKey>) -> Self {
        Self { keys }
    }

    /// Decapsulates a `message/ohttp-req` body.
    ///
    /// # Errors
    ///
    /// [`OhttpError::UnknownKeyId`], [`OhttpError::UnsupportedSuite`],
    /// [`OhttpError::OpenFailed`], [`OhttpError::BadMessage`].
    pub fn decapsulate(&self, message: &[u8]) -> Result<DecapsulatedRequest, OhttpError> {
        let key = self.select_key(message).map_err(|error| {
            // A one-shot message too short for its header cannot be opened.
            if matches!(error, OhttpError::Truncated) {
                OhttpError::OpenFailed
            } else {
                error
            }
        })?;
        let (plaintext, inner) = key
            .server()
            .decapsulate(message)
            .map_err(|_| OhttpError::OpenFailed)?;
        Ok(DecapsulatedRequest {
            request: BinaryRequest::decode(&plaintext)?,
            responder: Responder { inner },
        })
    }

    /// Reads the request header (key id and suite) and finds the key it names.
    ///
    /// [`OhttpError::Truncated`] if the header is incomplete.
    pub(crate) fn select_key(&self, message: &[u8]) -> Result<&GatewayKey, OhttpError> {
        let Some((key_id, suite)) = message.split_first() else {
            return Err(OhttpError::Truncated);
        };
        let key = self
            .keys
            .iter()
            .find(|key| key.key_id() == *key_id)
            .ok_or(OhttpError::UnknownKeyId { key_id: *key_id })?;
        let [
            kem_high,
            kem_low,
            kdf_high,
            kdf_low,
            aead_high,
            aead_low,
            ..,
        ] = *suite
        else {
            return Err(OhttpError::Truncated);
        };
        let pinned = u16::from_be_bytes([kem_high, kem_low]) == KEM_X25519
            && u16::from_be_bytes([kdf_high, kdf_low]) == KDF_HKDF_SHA256
            && u16::from_be_bytes([aead_high, aead_low]) == AEAD_AES_128_GCM;
        if pinned {
            Ok(key)
        } else {
            Err(OhttpError::UnsupportedSuite)
        }
    }
}

/// A decapsulated request and the context that seals its response.
pub struct DecapsulatedRequest {
    /// The inner request.
    pub request: BinaryRequest,
    /// Seals the response; consumed by use.
    pub responder: Responder,
}

/// The gateway's HPKE context for one response.
pub struct Responder {
    inner: ohttp::ServerResponse,
}

impl Responder {
    /// Seals the response as a `message/ohttp-res` body.
    ///
    /// # Errors
    ///
    /// [`OhttpError::BadMessage`] if the response cannot be encoded; [`OhttpError::Internal`].
    pub fn seal(
        self,
        response: &BinaryResponse,
        rng: &mut impl CryptoRng,
    ) -> Result<Vec<u8>, OhttpError> {
        let encoded = response.encode()?;
        self.inner
            .encapsulate(&encoded, rng)
            .map_err(|_| OhttpError::Internal)
    }

    /// As [`Responder::seal`], with the encoded response padded with zeros to exactly
    /// `padded_length` bytes (RFC 9292 §3.8), so its size says only which size class it is in.
    ///
    /// # Errors
    ///
    /// [`OhttpError::LongerThanPadding`] if the encoded response is longer than
    /// `padded_length`; otherwise as [`Responder::seal`].
    pub fn seal_padded(
        self,
        response: &BinaryResponse,
        padded_length: usize,
        rng: &mut impl CryptoRng,
    ) -> Result<Vec<u8>, OhttpError> {
        self.seal_padded_encoded(response.encode()?, padded_length, rng)
    }

    /// As [`Responder::seal`], with the encoded response padded with zeros to the smallest of
    /// [`RESPONSE_CLASSES`] that holds it: every response in one class has the same length, so
    /// an empty answer cannot be told from a full one in its class.
    ///
    /// # Errors
    ///
    /// [`OhttpError::LongerThanPadding`] if the encoded response is longer than the largest
    /// class; otherwise as [`Responder::seal`].
    pub fn seal_classed(
        self,
        response: &BinaryResponse,
        rng: &mut impl CryptoRng,
    ) -> Result<Vec<u8>, OhttpError> {
        let encoded = response.encode()?;
        let length = class_for(encoded.len(), &RESPONSE_CLASSES)?;
        self.seal_padded_encoded(encoded, length, rng)
    }

    fn seal_padded_encoded(
        self,
        encoded: Vec<u8>,
        padded_length: usize,
        rng: &mut impl CryptoRng,
    ) -> Result<Vec<u8>, OhttpError> {
        let encoded = padded(encoded, padded_length)?;
        self.inner
            .encapsulate(&encoded, rng)
            .map_err(|_| OhttpError::Internal)
    }
}
