// SPDX-License-Identifier: AGPL-3.0-only
//! One request, one response (RFC 9458).

use rand_core::CryptoRng;

use crate::stub::{Unimplemented, not_implemented};
use crate::{BinaryRequest, BinaryResponse, GatewayKey, KeyConfig, OhttpError};

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
    unimplemented: Unimplemented,
}

impl ResponseContext {
    /// Opens the `message/ohttp-res` body.
    ///
    /// # Errors
    ///
    /// [`OhttpError::OpenFailed`] if it does not decrypt; [`OhttpError::BadMessage`] if the
    /// inner response is malformed.
    pub fn open(self, response: &[u8]) -> Result<BinaryResponse, OhttpError> {
        let _ = response;
        match self.unimplemented {}
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
    not_implemented((config, request, rng))
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
        not_implemented((&self.keys, message))
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
    unimplemented: Unimplemented,
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
        let _ = (response, rng);
        match self.unimplemented {}
    }
}
