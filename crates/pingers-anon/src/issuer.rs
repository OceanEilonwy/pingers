// SPDX-License-Identifier: AGPL-3.0-only
//! Privacy Pass issuer and redemption check, server side (type 0x0001, issuer = origin).

use rand_core::CryptoRng;
use subtle::ConstantTimeEq as _;
use voprf::{BlindedElement, Group as _, VoprfServer};
use zeroize::Zeroizing;

use crate::AnonError;
use crate::pp::{self, Suite};

/// The issuer's VOPRF private key, rotated weekly (design C.1 `pp_key`). No `Debug` or `Clone`.
pub struct IssuerKey {
    /// The key; wiped on drop.
    server: VoprfServer<Suite>,
    /// The private scalar's encoding, for the key store.
    secret: Zeroizing<Vec<u8>>,
    /// The compressed public key.
    public_key: Vec<u8>,
    /// `SHA-256(public_key)`.
    key_id: [u8; 32],
}

impl IssuerKey {
    /// Generates a key.
    ///
    /// # Errors
    ///
    /// [`AnonError::Internal`] if key generation fails.
    pub fn generate(rng: &mut impl CryptoRng) -> Result<Self, AnonError> {
        let scalar = Suite::random_scalar(rng).map_err(|_| AnonError::Internal)?;
        let secret = Zeroizing::new(Suite::serialize_scalar(scalar).to_vec());
        Self::from_bytes(secret).map_err(|_| AnonError::Internal)
    }

    /// Loads a stored key.
    ///
    /// # Errors
    ///
    /// [`AnonError::NotAFieldElement`] if the bytes are not a valid P-384 scalar.
    pub fn from_bytes(bytes: Zeroizing<Vec<u8>>) -> Result<Self, AnonError> {
        let server = VoprfServer::new_with_key(&bytes).map_err(|_| AnonError::NotAFieldElement)?;
        let public_key = Suite::serialize_elem(server.get_public_key()).to_vec();
        let key_id = pp::key_id(&public_key);
        Ok(Self {
            server,
            secret: bytes,
            public_key,
            key_id,
        })
    }

    /// The private scalar (48 bytes, big-endian), for the server's key store only.
    #[must_use]
    pub fn expose_bytes(&self) -> &[u8] {
        &self.secret
    }

    /// The public key to publish at `GET /v1/pp/keys`.
    #[must_use]
    pub fn public_key(&self) -> Vec<u8> {
        self.public_key.clone()
    }

    /// The key id tokens name.
    #[must_use]
    pub fn key_id(&self) -> [u8; 32] {
        self.key_id
    }
}

/// Evaluates a batch request and proves it (batched issuance, RFC 9578). Quotas are the
/// caller's check (pingers-quota).
///
/// # Errors
///
/// [`AnonError::BadIssuance`] if the request is malformed, names another key, holds more than
/// 300 elements or an element that is not a valid point.
pub fn issue_tokens(
    key: &IssuerKey,
    request: &[u8],
    rng: &mut impl CryptoRng,
) -> Result<Vec<u8>, AnonError> {
    let decoded = pp::decode_request(request)?;
    if key.key_id.last() != Some(&decoded.truncated_key_id) {
        return Err(AnonError::BadIssuance);
    }
    let blinded = decoded
        .elements
        .iter()
        .map(|element| BlindedElement::<Suite>::deserialize(element))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| AnonError::BadIssuance)?;
    let evaluated = key
        .server
        .batch_blind_evaluate(rng, &blinded)
        .map_err(|_| AnonError::BadIssuance)?;
    let elements: Vec<_> = evaluated
        .messages
        .iter()
        .map(voprf::EvaluationElement::serialize)
        .collect();
    pp::encode_response(&elements, &evaluated.proof.serialize())
}

/// Verifies a redeemed token and returns its nonce, which the caller checks against and adds to
/// the spent set.
///
/// # Errors
///
/// [`AnonError::BadToken`] if it does not verify under `key`.
pub fn check_redemption(key: &IssuerKey, token: &[u8]) -> Result<[u8; 32], AnonError> {
    let parsed = pp::parse_token(token)?;
    if parsed.key_id != key.key_id {
        return Err(AnonError::BadToken);
    }
    let expected = key
        .server
        .evaluate(parsed.input)
        .map_err(|_| AnonError::BadToken)?;
    if bool::from(expected.as_slice().ct_eq(parsed.authenticator)) {
        Ok(parsed.nonce)
    } else {
        Err(AnonError::BadToken)
    }
}
