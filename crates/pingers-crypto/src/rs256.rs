// SPDX-License-Identifier: AGPL-3.0-only
//! RS256 compact JSON Web Signatures (RFC 7515 §7.1, RSASSA-PKCS1-v1_5 with SHA-256), for the
//! push relay's FCM service-account assertion (RFC 7523): Google's token endpoint requires it.
//!
//! It is not part of the Pingers protocol and has no domain-separation label: the header and
//! claims are Google's. It lives here because only this crate calls cryptographic primitives
//! (style guide §8), behind the `rs256` feature so only the push relay links it.
//!
//! The private key is parsed once, from text the caller holds in a [`Zeroizing`] buffer, into an
//! aws-lc-rs key pair that AWS-LC zeroizes when it is freed. [`Rs256Key`] implements neither
//! `Debug` nor `Clone`, and no error carries any part of the key.

use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use zeroize::Zeroizing;

use crate::CryptoError;

const PEM_BEGIN: &str = "-----BEGIN PRIVATE KEY-----";
const PEM_END: &str = "-----END PRIVATE KEY-----";

/// An RSA private key for RS256, held by AWS-LC (zeroized on free). No `Debug` or `Clone`.
pub struct Rs256Key(RsaKeyPair);

impl Rs256Key {
    /// Parses a PEM `PRIVATE KEY` block (PKCS #8) holding an RSA key of 2048 to 8192 bits.
    /// The base64 body may be wrapped with any whitespace.
    ///
    /// # Errors
    ///
    /// [`CryptoError::InvalidKey`] if there is no such block, its body is not base64, or it is
    /// not a PKCS #8 RSA key of an accepted size.
    pub fn from_pkcs8_pem(pem: &str) -> Result<Self, CryptoError> {
        let der = pkcs8_der(pem)?;
        RsaKeyPair::from_pkcs8(&der)
            .map(Self)
            .map_err(|_| CryptoError::InvalidKey)
    }
}

/// The DER inside a `PRIVATE KEY` PEM block, in a buffer wiped on drop.
fn pkcs8_der(pem: &str) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    let (_, rest) = pem.split_once(PEM_BEGIN).ok_or(CryptoError::InvalidKey)?;
    let (body, _) = rest.split_once(PEM_END).ok_or(CryptoError::InvalidKey)?;
    // Sized up front so it never reallocates and leaves a copy behind.
    let mut base64 = Zeroizing::new(String::with_capacity(body.len()));
    base64.extend(
        body.chars()
            .filter(|character| !character.is_ascii_whitespace()),
    );
    STANDARD
        .decode(base64.as_bytes())
        .map(Zeroizing::new)
        .map_err(|_| CryptoError::InvalidKey)
}

/// The compact serialisation `base64url(header) "." base64url(claims) "." base64url(signature)`,
/// signed RS256 with `key`. `header` and `claims` are the JSON the caller built; they are
/// encoded, not checked.
///
/// # Errors
///
/// [`CryptoError::Internal`] if AWS-LC refuses to sign (it does not for a parsed key).
pub fn sign_compact_jws_rs256(
    key: &Rs256Key,
    header: &[u8],
    claims: &[u8],
) -> Result<Zeroizing<String>, CryptoError> {
    let signing_input = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(header),
        URL_SAFE_NO_PAD.encode(claims)
    );
    let mut signature = vec![0; key.0.public_modulus_len()];
    // PKCS #1 v1.5 signing is deterministic; aws-lc-rs takes a generator for other schemes.
    key.0
        .sign(
            &RSA_PKCS1_SHA256,
            &SystemRandom::new(),
            signing_input.as_bytes(),
            &mut signature,
        )
        .map_err(|_| CryptoError::Internal)?;
    Ok(Zeroizing::new(format!(
        "{signing_input}.{}",
        URL_SAFE_NO_PAD.encode(signature)
    )))
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use std::sync::LazyLock;

    use aws_lc_rs::signature::{RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey};

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// One throwaway 2048-bit key for the whole run: generating RSA keys is slow.
    static TEST_KEY: LazyLock<Option<rcgen::KeyPair>> =
        LazyLock::new(|| rcgen::KeyPair::generate_for(&rcgen::PKCS_RSA_SHA256).ok());

    fn test_key() -> Result<&'static rcgen::KeyPair, &'static str> {
        TEST_KEY.as_ref().ok_or("cannot generate the test RSA key")
    }

    fn verifies(jws: &str) -> Result<bool, Box<dyn std::error::Error>> {
        let (signing_input, signature) = jws.rsplit_once('.').ok_or("no signature")?;
        let public_key =
            UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, test_key()?.public_key_raw());
        Ok(public_key
            .verify(
                signing_input.as_bytes(),
                &URL_SAFE_NO_PAD.decode(signature)?,
            )
            .is_ok())
    }

    #[test]
    fn the_signature_verifies_over_the_encoded_header_and_claims() -> TestResult {
        let key = Rs256Key::from_pkcs8_pem(&test_key()?.serialize_pem())?;
        let jws = sign_compact_jws_rs256(&key, br#"{"alg":"RS256"}"#, br#"{"iss":"a"}"#)?;
        let mut parts = jws.split('.');
        assert_eq!(parts.next(), Some("eyJhbGciOiJSUzI1NiJ9"));
        assert_eq!(parts.next(), Some("eyJpc3MiOiJhIn0"));
        // 2048 bits: 256 bytes, 342 unpadded base64url characters.
        assert_eq!(parts.next().map(str::len), Some(342));
        assert_eq!(parts.next(), None);
        assert!(verifies(&jws)?);
        Ok(())
    }

    #[test]
    fn changed_claims_do_not_verify_under_the_old_signature() -> TestResult {
        let key = Rs256Key::from_pkcs8_pem(&test_key()?.serialize_pem())?;
        let jws = sign_compact_jws_rs256(&key, b"{}", br#"{"exp":1}"#)?;
        let (_, signature) = jws.rsplit_once('.').ok_or("no signature")?;
        let other = sign_compact_jws_rs256(&key, b"{}", br#"{"exp":2}"#)?;
        let (other_input, _) = other.rsplit_once('.').ok_or("no signature")?;
        assert!(!verifies(&format!("{other_input}.{signature}"))?);
        Ok(())
    }

    #[test]
    fn a_key_that_is_not_pem_pkcs8_rsa_is_refused() -> TestResult {
        let pem = test_key()?.serialize_pem();
        let ec_key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)?.serialize_pem();
        for bad in [
            String::new(),
            pem.replace(PEM_BEGIN, ""),
            pem.replace(PEM_END, ""),
            pem.replace(&format!("{PEM_BEGIN}\n"), &format!("{PEM_BEGIN}\n!")),
            format!("{PEM_BEGIN}\nAAAA\n{PEM_END}\n"),
            ec_key,
        ] {
            assert!(matches!(
                Rs256Key::from_pkcs8_pem(&bad),
                Err(CryptoError::InvalidKey)
            ));
        }
        Ok(())
    }

    #[test]
    fn the_pem_body_may_be_wrapped_with_any_whitespace() -> TestResult {
        let pem = test_key()?.serialize_pem().replace('\n', "\r\n  ");
        assert!(Rs256Key::from_pkcs8_pem(&pem).is_ok());
        Ok(())
    }
}
