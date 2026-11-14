// SPDX-License-Identifier: AGPL-3.0-only
//! The account key: one Ed25519 key per device, also its MLS leaf signature key (design B.1).

use core::fmt;

use ed25519_dalek::SigningKey;
use pingers_proto::AccountId;
use sha2::{Digest as _, Sha256};

use crate::secret::{random_secret, secret_key};
use crate::{CryptoError, labels, primitives};

secret_key!(
    /// The account's Ed25519 secret key (32-byte seed).
    ///
    /// `from_bytes` and `expose_bytes` are public because pingers-store keeps it in the
    /// encrypted database and pingers-mls builds the MLS credential from it.
    AccountSigningKey,
    32,
    pub
);
random_secret!(AccountSigningKey);

impl AccountSigningKey {
    /// The matching public key.
    #[must_use]
    pub fn public_key(&self) -> AccountPublicKey {
        AccountPublicKey(
            SigningKey::from_bytes(self.expose_bytes())
                .verifying_key()
                .to_bytes(),
        )
    }
}

/// An account's Ed25519 public key: its identity in every Party roster.
/// `Debug` does not print it (style guide §9).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct AccountPublicKey([u8; 32]);

impl AccountPublicKey {
    /// Wraps key bytes from a roster or the wire. Validity is checked when verifying.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The key bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for AccountPublicKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AccountPublicKey(redacted)")
    }
}

/// An Ed25519 signature.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Signature([u8; 64]);

impl Signature {
    /// Wraps signature bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 64]) -> Self {
        Self(bytes)
    }

    /// The signature bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 64] {
        &self.0
    }
}

impl fmt::Debug for Signature {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Signature(..)")
    }
}

/// `account_id = SHA-256(pk)[..16]` (design B.1).
#[must_use]
pub fn account_id(public_key: &AccountPublicKey) -> AccountId {
    let digest = Sha256::digest(public_key.as_bytes());
    let mut id = [0; 16];
    for (target, source) in id.iter_mut().zip(digest.iter()) {
        *target = *source;
    }
    AccountId::from_bytes(id)
}

/// The proof of key possession in `POST /v1/accounts` (design D): a signature over
/// `"pingers/v1/register" || public key || server origin`.
///
/// # Errors
///
/// [`CryptoError::Internal`] if signing fails.
pub fn sign_registration(
    signing_key: &AccountSigningKey,
    server: &pingers_proto::ServerId,
) -> Result<Signature, CryptoError> {
    let message = registration_message(&signing_key.public_key(), server);
    Ok(Signature(primitives::ed25519_sign(
        signing_key.expose_bytes(),
        &message,
    )))
}

/// Checks a registration proof (server side).
///
/// # Errors
///
/// [`CryptoError::BadSignature`] if it does not verify for this key and server.
pub fn verify_registration(
    public_key: &AccountPublicKey,
    server: &pingers_proto::ServerId,
    signature: &Signature,
) -> Result<(), CryptoError> {
    primitives::ed25519_verify(
        public_key.as_bytes(),
        &registration_message(public_key, server),
        signature.as_bytes(),
    )
}

/// `"pingers/v1/register" || public key || server origin`. The origin is the only
/// variable-length field and comes last, so no length prefix is needed.
pub(crate) fn registration_message(
    public_key: &AccountPublicKey,
    server: &pingers_proto::ServerId,
) -> Vec<u8> {
    [
        labels::REGISTRATION,
        public_key.as_bytes(),
        server.as_str().as_bytes(),
    ]
    .concat()
}

/// The signed challenge of `POST /v1/auth` (design D, auth mode Acct): a signature over
/// `"pingers/v1/auth" || public key || be64(bucket index) || server origin`. The challenge is
/// the 15-minute bucket, so no round trip is needed to fetch one; the server accepts the
/// current bucket, or the previous one shortly after a rollover.
///
/// # Errors
///
/// [`CryptoError::Internal`] if signing fails.
pub fn sign_auth_challenge(
    signing_key: &AccountSigningKey,
    server: &pingers_proto::ServerId,
    bucket: pingers_time::Bucket,
) -> Result<Signature, CryptoError> {
    let message = auth_challenge_message(&signing_key.public_key(), server, bucket);
    Ok(Signature(primitives::ed25519_sign(
        signing_key.expose_bytes(),
        &message,
    )))
}

/// Checks a signed auth challenge (server side).
///
/// # Errors
///
/// [`CryptoError::BadSignature`] if it does not verify for this key, server and bucket.
pub fn verify_auth_challenge(
    public_key: &AccountPublicKey,
    server: &pingers_proto::ServerId,
    bucket: pingers_time::Bucket,
    signature: &Signature,
) -> Result<(), CryptoError> {
    primitives::ed25519_verify(
        public_key.as_bytes(),
        &auth_challenge_message(public_key, server, bucket),
        signature.as_bytes(),
    )
}

/// `"pingers/v1/auth" || public key || be64(bucket) || server origin`; the origin comes last,
/// so no length prefix is needed.
fn auth_challenge_message(
    public_key: &AccountPublicKey,
    server: &pingers_proto::ServerId,
    bucket: pingers_time::Bucket,
) -> Vec<u8> {
    [
        labels::AUTH_CHALLENGE,
        public_key.as_bytes(),
        &bucket.index().to_be_bytes(),
        server.as_str().as_bytes(),
    ]
    .concat()
}

secret_key!(
    /// The bearer of design D's Acct mode: 32 random bytes the server hands out for a signed
    /// challenge, valid for an hour. `from_bytes` and `expose_bytes` are public because the
    /// server sends it to the device (hex in `AuthResponse`) and parses it back from the
    /// `Authorization` header.
    BearerToken,
    32,
    pub
);
random_secret!(BearerToken);

/// What the server stores in place of a bearer, so its database never holds a usable one:
/// `SHA-256("pingers/v1/bearer" || bearer)`. A bearer is 32 random bytes, so no key is
/// needed to stop guessing. `Debug` does not print it.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct BearerDigest([u8; 32]);

impl BearerDigest {
    /// Wraps digest bytes read back from the store.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The digest bytes, for the store.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for BearerDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BearerDigest(redacted)")
    }
}

/// The digest a bearer is stored and looked up by (server side).
#[must_use]
pub fn bearer_digest(bearer: &BearerToken) -> BearerDigest {
    BearerDigest(primitives::sha256(&[labels::BEARER, bearer.expose_bytes()]))
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    #[test]
    fn a_bearer_digest_does_not_print() {
        let printed = format!("{:?}", BearerDigest::from_bytes([0xAB; 32]));
        assert_eq!(printed, "BearerDigest(redacted)");
    }

    #[test]
    fn account_id_is_the_first_16_bytes_of_the_key_hash() {
        // SHA-256 of 32 zero bytes begins 66 68 7a ad f8 62 bd 77 6c 8f c1 8b 8e 9f 8e 20.
        let id = account_id(&AccountPublicKey::from_bytes([0; 32]));
        assert_eq!(
            id,
            AccountId::from_bytes([
                0x66, 0x68, 0x7a, 0xad, 0xf8, 0x62, 0xbd, 0x77, 0x6c, 0x8f, 0xc1, 0x8b, 0x8e, 0x9f,
                0x8e, 0x20
            ])
        );
    }

    #[test]
    fn auth_challenges_verify_only_for_their_bucket_server_and_key() -> Result<(), CryptoError> {
        let key = AccountSigningKey::from_bytes(zeroize::Zeroizing::new([7; 32]));
        let other = AccountSigningKey::from_bytes(zeroize::Zeroizing::new([8; 32]));
        let server = pingers_proto::ServerId::new("https://pingers.example".to_owned())
            .map_err(|_| CryptoError::Internal)?;
        let elsewhere = pingers_proto::ServerId::new("https://other.example".to_owned())
            .map_err(|_| CryptoError::Internal)?;
        let bucket = pingers_time::Bucket::from_index(1_000).map_err(|_| CryptoError::Internal)?;
        let next = pingers_time::Bucket::from_index(1_001).map_err(|_| CryptoError::Internal)?;
        let signature = sign_auth_challenge(&key, &server, bucket)?;
        verify_auth_challenge(&key.public_key(), &server, bucket, &signature)?;
        for (public, server, bucket) in [
            (key.public_key(), &server, next),
            (key.public_key(), &elsewhere, bucket),
            (other.public_key(), &server, bucket),
        ] {
            assert!(matches!(
                verify_auth_challenge(&public, server, bucket, &signature),
                Err(CryptoError::BadSignature)
            ));
        }
        Ok(())
    }
}
