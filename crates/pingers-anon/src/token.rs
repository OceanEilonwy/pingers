// SPDX-License-Identifier: AGPL-3.0-only
//! Privacy Pass client: token requests and redemption (RFC 9576/9578, type 0x0001).

use rand_core::CryptoRng;
use voprf::{EvaluationElement, Group as _, Proof, VoprfClient};
use zeroize::Zeroizing;

use crate::AnonError;
use crate::pp::{self, NONCE_LEN, Suite, TokenCount};

/// A batch request in flight: blinded elements sent, blinds kept to finalise the answer.
pub struct PendingIssuance {
    /// The encoded request.
    request: Vec<u8>,
    /// The issuer's public key, which the batch proof must verify under.
    public_key: <Suite as voprf::Group>::Elem,
    /// `SHA-256(public key)`.
    key_id: [u8; 32],
    /// One nonce per token, in request order.
    nonces: Vec<[u8; NONCE_LEN]>,
    /// One blind per token, in request order.
    clients: Vec<VoprfClient<Suite>>,
}

impl PendingIssuance {
    /// The request body for `POST /v1/pp/issue`.
    #[must_use]
    pub fn request(&self) -> &[u8] {
        &self.request
    }

    /// Verifies the issuer's batch proof and unblinds the tokens.
    ///
    /// # Errors
    ///
    /// [`AnonError::BadIssuance`] if the response does not match or its proof fails.
    pub fn finish(self, response: &[u8]) -> Result<Vec<PrivacyPassToken>, AnonError> {
        let decoded = pp::decode_response(response)?;
        if decoded.elements.len() != self.clients.len() {
            return Err(AnonError::BadIssuance);
        }
        let elements = decoded
            .elements
            .iter()
            .map(|element| EvaluationElement::<Suite>::deserialize(element))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| AnonError::BadIssuance)?;
        let proof =
            Proof::<Suite>::deserialize(decoded.proof).map_err(|_| AnonError::BadIssuance)?;
        let inputs: Vec<Vec<u8>> = self
            .nonces
            .iter()
            .map(|nonce| pp::token_input(nonce, &self.key_id))
            .collect();
        let outputs =
            VoprfClient::batch_finalize(&inputs, &self.clients, &elements, &proof, self.public_key)
                .map_err(|_| AnonError::BadIssuance)?;
        inputs
            .iter()
            .zip(outputs)
            .map(|(input, output)| {
                let output = output.map_err(|_| AnonError::BadIssuance)?;
                let mut token = Vec::with_capacity(pp::TOKEN_LEN);
                token.extend_from_slice(input);
                token.extend_from_slice(&output);
                Ok(PrivacyPassToken(Zeroizing::new(token)))
            })
            .collect()
    }
}

/// Starts a batch request against the issuer's public key (from `GET /v1/pp/keys`).
///
/// # Errors
///
/// [`AnonError::BadIssuance`] if the issuer key is invalid; [`AnonError::Internal`].
pub fn begin_token_issuance(
    issuer_public_key: &[u8],
    count: TokenCount,
    rng: &mut impl CryptoRng,
) -> Result<PendingIssuance, AnonError> {
    let public_key =
        Suite::deserialize_elem(issuer_public_key).map_err(|_| AnonError::BadIssuance)?;
    let key_id = pp::key_id(issuer_public_key);
    let capacity = usize::from(count.get());
    let mut nonces = Vec::with_capacity(capacity);
    let mut clients = Vec::with_capacity(capacity);
    let mut blinded = Vec::with_capacity(capacity);
    for _ in 0..count.get() {
        let mut nonce = [0_u8; NONCE_LEN];
        rng.fill_bytes(&mut nonce);
        let blind = VoprfClient::<Suite>::blind(&pp::token_input(&nonce, &key_id), rng)
            .map_err(|_| AnonError::Internal)?;
        nonces.push(nonce);
        clients.push(blind.state);
        blinded.push(blind.message.serialize());
    }
    let truncated_key_id = key_id.last().copied().ok_or(AnonError::Internal)?;
    Ok(PendingIssuance {
        request: pp::encode_request(truncated_key_id, &blinded)?,
        public_key,
        key_id,
        nonces,
        clients,
    })
}

/// One unspent token, shared with the app's background handlers through the `pp_token` table.
/// `Debug` does not print it, it is zeroized on drop, and it is neither `Clone` nor comparable:
/// a token is a bearer credential spent once (style guide §8).
pub struct PrivacyPassToken(Zeroizing<Vec<u8>>);

impl PrivacyPassToken {
    /// Loads a stored token.
    #[must_use]
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(Zeroizing::new(bytes))
    }

    /// The token encoding, for storage.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// The `Authorization: PrivateToken token=...` header value that spends it (RFC 9577),
    /// zeroized on drop like the token.
    #[must_use]
    pub fn authorization(&self) -> Zeroizing<String> {
        Zeroizing::new(format!("PrivateToken token=\"{}\"", pp::base64url(&self.0)))
    }
}

impl core::fmt::Debug for PrivacyPassToken {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("PrivacyPassToken(redacted)")
    }
}
