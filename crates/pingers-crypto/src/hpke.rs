// SPDX-License-Identifier: AGPL-3.0-only
//! HPKE (RFC 9180) base mode, single shot, through hpke-rs.
//!
//! hpke-rs draws its randomness from a PRNG owned by the `Hpke` value, and the `RustCrypto`
//! provider seeds that PRNG from the operating system. The style guide requires randomness to
//! be passed in (§6), and test vectors must be reproducible, so this module wraps the provider:
//! [`Provider`] delegates every primitive to `HpkeRustCrypto` but uses a [`Tape`] PRNG that
//! replays bytes drawn beforehand from the caller's RNG. The HPKE key schedule itself is
//! hpke-rs's.
//!
//! Sealed format everywhere: `enc || ciphertext || tag`, with empty AAD (bindings go in
//! `info`).

use core::fmt;

use hpke_rs::hpke_types::{
    AeadAlgorithm, KdfAlgorithm, KemAlgorithm, SingleStageKdfAlgorithm, TwoStageKdfAlgorithm,
};
use hpke_rs::rustcrypto::HpkeRustCrypto;
use hpke_rs::{Hpke, HpkePrivateKey, HpkePublicKey, Mode};
use hpke_rs_crypto::HpkeCrypto;
use hpke_rs_crypto::error::Error as ProviderError;
use rand_core::CryptoRng;
use zeroize::Zeroizing;

use crate::CryptoError;

/// The HPKE ciphersuites Pingers uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Suite {
    /// Sealed push tokens: DHKEM(X25519, HKDF-SHA256), HKDF-SHA256, AES-128-GCM (design D,
    /// `RelayConfig`).
    PushRelay,
    /// Location grants and asks: DHKEM(X25519, HKDF-SHA256), HKDF-SHA256,
    /// ChaCha20-Poly1305, matching the Party's MLS suite 0x0003.
    Grant,
    /// Location grants and asks in post-quantum Parties: X-Wing (draft 06, code point
    /// 0x647a), HKDF-SHA256, ChaCha20-Poly1305 (design B.8).
    #[cfg(feature = "x-wing")]
    GrantXWing,
}

impl Suite {
    const fn kem(self) -> KemAlgorithm {
        match self {
            Self::PushRelay | Self::Grant => KemAlgorithm::DhKem25519,
            #[cfg(feature = "x-wing")]
            Self::GrantXWing => KemAlgorithm::XWingDraft06,
        }
    }

    const fn aead(self) -> AeadAlgorithm {
        match self {
            Self::PushRelay => AeadAlgorithm::Aes128Gcm,
            Self::Grant => AeadAlgorithm::ChaCha20Poly1305,
            #[cfg(feature = "x-wing")]
            Self::GrantXWing => AeadAlgorithm::ChaCha20Poly1305,
        }
    }

    /// Length of the encapsulated key at the front of a sealed message.
    pub(crate) const fn enc_length(self) -> usize {
        match self {
            Self::PushRelay | Self::Grant => 32,
            #[cfg(feature = "x-wing")]
            Self::GrantXWing => x_wing::CIPHERTEXT_SIZE,
        }
    }

    /// Length of a public key.
    pub(crate) const fn public_key_length(self) -> usize {
        match self {
            Self::PushRelay | Self::Grant => 32,
            #[cfg(feature = "x-wing")]
            Self::GrantXWing => x_wing::ENCAPSULATION_KEY_SIZE,
        }
    }

    /// Bytes of randomness one seal consumes: the X25519 `ikmE`, or X-Wing's encapsulation
    /// randomness.
    pub(crate) const fn seal_randomness_length(self) -> usize {
        match self {
            Self::PushRelay | Self::Grant => 32,
            #[cfg(feature = "x-wing")]
            Self::GrantXWing => x_wing::ENCAPSULATION_RANDOMNESS_SIZE,
        }
    }

    /// The bytes a sealed message adds to its plaintext.
    pub(crate) const fn overhead(self) -> usize {
        self.enc_length()
            .saturating_add(crate::primitives::TAG_LENGTH)
    }

    fn hpke(self, randomness: Option<Zeroizing<Vec<u8>>>) -> Hpke<Provider> {
        Hpke::new_with_rng(
            Mode::Base,
            self.kem(),
            KdfAlgorithm::HkdfSha256,
            self.aead(),
            Tape {
                bytes: randomness.unwrap_or_default(),
                position: 0,
            },
        )
    }
}

/// A key pair as bytes, before it is wrapped in the caller's types.
pub(crate) struct KeyPair {
    pub(crate) secret: Zeroizing<[u8; 32]>,
    pub(crate) public: Vec<u8>,
}

/// `DeriveKeyPair(ikm)` (RFC 9180 §7.1.3) for `suite`'s KEM. Both KEMs used here have 32-byte
/// secret keys (X-Wing's is its seed).
pub(crate) fn derive_key_pair(suite: Suite, ikm: &[u8; 32]) -> Result<KeyPair, CryptoError> {
    let pair = suite
        .hpke(None)
        .derive_key_pair(ikm)
        .map_err(|_error| CryptoError::Internal)?;
    let (secret, public) = pair.into_keys();
    let secret_bytes: [u8; 32] = secret
        .as_slice()
        .try_into()
        .map_err(|_length| CryptoError::Internal)?;
    Ok(KeyPair {
        secret: Zeroizing::new(secret_bytes),
        public: public.as_slice().to_vec(),
    })
}

/// The X25519 public key of a secret key (both X25519 suites; X-Wing keys are derived with
/// [`derive_key_pair`] instead).
pub(crate) fn public_key(suite: Suite, secret_key: &[u8; 32]) -> Result<[u8; 32], CryptoError> {
    let public = Provider::secret_to_public(suite.kem(), secret_key)
        .map_err(|_error| CryptoError::Internal)?;
    public
        .as_slice()
        .try_into()
        .map_err(|_length| CryptoError::Internal)
}

/// Who a message is sealed to.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Recipient<'key> {
    /// The suite, which must match the key.
    pub(crate) suite: Suite,
    /// The recipient's public key.
    pub(crate) public_key: &'key [u8],
}

/// Seals with randomness drawn from `rng`.
pub(crate) fn seal(
    recipient: Recipient<'_>,
    info: &[u8],
    plaintext: &[u8],
    rng: &mut impl CryptoRng,
) -> Result<Vec<u8>, CryptoError> {
    let mut randomness = Zeroizing::new(vec![0; recipient.suite.seal_randomness_length()]);
    rng.fill_bytes(randomness.as_mut_slice());
    seal_with_randomness(recipient, info, plaintext, randomness)
}

/// Seals with the given encapsulation randomness, for test vectors.
pub(crate) fn seal_with_randomness(
    recipient: Recipient<'_>,
    info: &[u8],
    plaintext: &[u8],
    randomness: Zeroizing<Vec<u8>>,
) -> Result<Vec<u8>, CryptoError> {
    let Recipient { suite, public_key } = recipient;
    if public_key.len() != suite.public_key_length() {
        return Err(CryptoError::InvalidKey);
    }
    let mut hpke = suite.hpke(Some(randomness));
    let (enc, ciphertext) = hpke
        .seal(
            &HpkePublicKey::new(public_key.to_vec()),
            info,
            &[],
            plaintext,
            None,
            None,
            None,
        )
        .map_err(|error| {
            // A public key that is a low-order point fails the X25519 all-zero check, and an
            // X-Wing key that does not decode fails encapsulation; nothing else about the
            // inputs can make sealing fail.
            if matches!(
                error,
                hpke_rs::HpkeError::CryptoError(_) | hpke_rs::HpkeError::InvalidInput
            ) {
                CryptoError::InvalidKey
            } else {
                CryptoError::Internal
            }
        })?;
    let mut sealed = enc;
    sealed.extend_from_slice(&ciphertext);
    Ok(sealed)
}

/// Opens `enc || ciphertext`. Every failure is [`CryptoError::OpenFailed`]: callers trial-open,
/// and must not learn more than "not for this key".
pub(crate) fn open(
    suite: Suite,
    secret_key: &[u8; 32],
    info: &[u8],
    sealed: &[u8],
) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    let (enc, ciphertext) = sealed
        .split_at_checked(suite.enc_length())
        .ok_or(CryptoError::OpenFailed)?;
    let plaintext = suite
        .hpke(None)
        .open(
            enc,
            &HpkePrivateKey::new(secret_key.to_vec()),
            info,
            &[],
            ciphertext,
            None,
            None,
            None,
        )
        .map_err(|_error| CryptoError::OpenFailed)?;
    Ok(Zeroizing::new(plaintext))
}

/// hpke-rs's `RustCrypto` provider with a caller-fed PRNG.
#[derive(Debug)]
pub(crate) struct Provider;

/// A PRNG that hands out bytes drawn beforehand, and fails once they run out. One seal draws
/// exactly [`Suite::seal_randomness_length`] bytes, so running out means hpke-rs changed what
/// it consumes, and sealing must fail rather than fall back to anything weaker.
pub(crate) struct Tape {
    bytes: Zeroizing<Vec<u8>>,
    position: usize,
}

/// The [`Tape`] ran out.
#[derive(Debug)]
pub(crate) struct TapeExhausted;

impl fmt::Display for TapeExhausted {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("HPKE randomness exhausted")
    }
}

impl core::error::Error for TapeExhausted {}

impl rand_core::TryRng for Tape {
    type Error = TapeExhausted;

    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        let mut bytes = [0; 4];
        self.try_fill_bytes(&mut bytes)?;
        Ok(u32::from_le_bytes(bytes))
    }

    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        let mut bytes = [0; 8];
        self.try_fill_bytes(&mut bytes)?;
        Ok(u64::from_le_bytes(bytes))
    }

    fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), Self::Error> {
        let end = self
            .position
            .checked_add(destination.len())
            .ok_or(TapeExhausted)?;
        let source = self.bytes.get(self.position..end).ok_or(TapeExhausted)?;
        destination.copy_from_slice(source);
        self.position = end;
        Ok(())
    }
}

impl rand_core::TryCryptoRng for Tape {}

impl HpkeCrypto for Provider {
    type HpkePrng = Tape;

    fn name() -> String {
        String::from("Pingers(RustCrypto)")
    }

    fn supports_kdf(algorithm: KdfAlgorithm) -> Result<(), ProviderError> {
        if matches!(algorithm, KdfAlgorithm::HkdfSha256) {
            Ok(())
        } else {
            Err(ProviderError::UnknownKdfAlgorithm)
        }
    }

    fn supports_kem(algorithm: KemAlgorithm) -> Result<(), ProviderError> {
        let x_wing = cfg!(feature = "x-wing") && matches!(algorithm, KemAlgorithm::XWingDraft06);
        if matches!(algorithm, KemAlgorithm::DhKem25519) || x_wing {
            Ok(())
        } else {
            Err(ProviderError::UnknownKemAlgorithm)
        }
    }

    fn supports_aead(algorithm: AeadAlgorithm) -> Result<(), ProviderError> {
        if matches!(
            algorithm,
            AeadAlgorithm::Aes128Gcm | AeadAlgorithm::ChaCha20Poly1305
        ) {
            Ok(())
        } else {
            Err(ProviderError::UnknownAeadAlgorithm)
        }
    }

    fn kdf_extract(
        algorithm: TwoStageKdfAlgorithm,
        salt: &[u8],
        ikm: &[u8],
    ) -> Result<Vec<u8>, ProviderError> {
        HpkeRustCrypto::kdf_extract(algorithm, salt, ikm)
    }

    fn kdf_expand(
        algorithm: TwoStageKdfAlgorithm,
        prk: &[u8],
        info: &[u8],
        output_size: usize,
    ) -> Result<Vec<u8>, ProviderError> {
        HpkeRustCrypto::kdf_expand(algorithm, prk, info, output_size)
    }

    fn kdf_derive(
        _algorithm: SingleStageKdfAlgorithm,
        _ikm: &[u8],
        _length: usize,
    ) -> Result<Vec<u8>, ProviderError> {
        // Single-stage (SHAKE) KDFs belong to suites Pingers does not use.
        Err(ProviderError::UnknownKdfAlgorithm)
    }

    fn dh(
        algorithm: KemAlgorithm,
        public_key: &[u8],
        secret_key: &[u8],
    ) -> Result<Vec<u8>, ProviderError> {
        HpkeRustCrypto::dh(algorithm, public_key, secret_key)
    }

    fn secret_to_public(
        algorithm: KemAlgorithm,
        secret_key: &[u8],
    ) -> Result<Vec<u8>, ProviderError> {
        HpkeRustCrypto::secret_to_public(algorithm, secret_key)
    }

    fn kem_key_gen(
        _algorithm: KemAlgorithm,
        _prng: &mut Self::HpkePrng,
    ) -> Result<(Vec<u8>, Vec<u8>), ProviderError> {
        // Key pairs are always derived from caller randomness (`derive_key_pair`).
        Err(ProviderError::UnsupportedKemOperation)
    }

    #[cfg_attr(
        not(feature = "x-wing"),
        expect(unused_variables, reason = "only X-Wing uses the KEM interface")
    )]
    fn kem_key_gen_derand(
        algorithm: KemAlgorithm,
        seed: &[u8],
    ) -> Result<(Vec<u8>, Vec<u8>), ProviderError> {
        match algorithm {
            #[cfg(feature = "x-wing")]
            KemAlgorithm::XWingDraft06 => x_wing_kem::key_gen(seed),
            _other => Err(ProviderError::UnsupportedKemOperation),
        }
    }

    fn kem_encaps(
        _algorithm: KemAlgorithm,
        _public_key: &[u8],
        _prng: &mut Self::HpkePrng,
    ) -> Result<(Vec<u8>, Vec<u8>), ProviderError> {
        // hpke-rs calls this only when `kem_encaps_derand` is unsupported.
        Err(ProviderError::UnsupportedKemOperation)
    }

    #[cfg_attr(
        not(feature = "x-wing"),
        expect(unused_variables, reason = "only X-Wing uses the KEM interface")
    )]
    fn kem_encaps_derand(
        algorithm: KemAlgorithm,
        public_key: &[u8],
        randomness: &[u8],
    ) -> Result<(Vec<u8>, Vec<u8>), ProviderError> {
        match algorithm {
            #[cfg(feature = "x-wing")]
            KemAlgorithm::XWingDraft06 => x_wing_kem::encaps(public_key, randomness),
            _other => Err(ProviderError::UnsupportedKemOperation),
        }
    }

    #[cfg_attr(
        not(feature = "x-wing"),
        expect(unused_variables, reason = "only X-Wing uses the KEM interface")
    )]
    fn kem_decaps(
        algorithm: KemAlgorithm,
        ciphertext: &[u8],
        secret_key: &[u8],
    ) -> Result<Vec<u8>, ProviderError> {
        match algorithm {
            #[cfg(feature = "x-wing")]
            KemAlgorithm::XWingDraft06 => x_wing_kem::decaps(ciphertext, secret_key),
            _other => Err(ProviderError::UnsupportedKemOperation),
        }
    }

    fn dh_validate_sk(
        _algorithm: KemAlgorithm,
        _secret_key: &[u8],
    ) -> Result<Vec<u8>, ProviderError> {
        // Only the NIST curves need secret-key validation; X25519 accepts every 32 bytes.
        Err(ProviderError::UnknownKemAlgorithm)
    }

    fn aead_seal(
        algorithm: AeadAlgorithm,
        key: &[u8],
        nonce: &[u8],
        aad: &[u8],
        message: &[u8],
    ) -> Result<Vec<u8>, ProviderError> {
        HpkeRustCrypto::aead_seal(algorithm, key, nonce, aad, message)
    }

    fn aead_open(
        algorithm: AeadAlgorithm,
        key: &[u8],
        nonce: &[u8],
        aad: &[u8],
        message: &[u8],
    ) -> Result<Vec<u8>, ProviderError> {
        HpkeRustCrypto::aead_open(algorithm, key, nonce, aad, message)
    }
}

/// X-Wing (draft-connolly-cfrg-xwing-kem-06) as an HPKE KEM, with the 32-byte seed as the
/// secret key, as hpke-rs's own providers do.
#[cfg(feature = "x-wing")]
mod x_wing_kem {
    use hpke_rs_crypto::error::Error as ProviderError;
    use x_wing::{Decapsulate as _, Decapsulator as _, KeyExport as _, TryKeyInit as _};

    pub(super) fn key_gen(seed: &[u8]) -> Result<(Vec<u8>, Vec<u8>), ProviderError> {
        let seed: [u8; x_wing::DECAPSULATION_KEY_SIZE] = seed
            .try_into()
            .map_err(|_length| ProviderError::KemInvalidSecretKey)?;
        let decapsulation = x_wing::DecapsulationKey::from(seed);
        let encapsulation = decapsulation.encapsulation_key();
        Ok((
            encapsulation.to_bytes().as_slice().to_vec(),
            decapsulation.as_bytes().to_vec(),
        ))
    }

    pub(super) fn encaps(
        public_key: &[u8],
        randomness: &[u8],
    ) -> Result<(Vec<u8>, Vec<u8>), ProviderError> {
        let key = x_wing::EncapsulationKey::new_from_slice(public_key)
            .map_err(|_error| ProviderError::KemInvalidPublicKey)?;
        let randomness: [u8; x_wing::ENCAPSULATION_RANDOMNESS_SIZE] = randomness
            .try_into()
            .map_err(|_length| ProviderError::InsufficientRandomness)?;
        let (ciphertext, shared) = key.encapsulate_deterministic(&randomness.into());
        Ok((shared.as_slice().to_vec(), ciphertext.as_slice().to_vec()))
    }

    pub(super) fn decaps(ciphertext: &[u8], secret_key: &[u8]) -> Result<Vec<u8>, ProviderError> {
        let seed: [u8; x_wing::DECAPSULATION_KEY_SIZE] = secret_key
            .try_into()
            .map_err(|_length| ProviderError::KemInvalidSecretKey)?;
        let key = x_wing::DecapsulationKey::from(seed);
        let ciphertext = x_wing::Ciphertext::try_from(ciphertext)
            .map_err(|_error| ProviderError::KemInvalidCiphertext)?;
        Ok(key.decapsulate(&ciphertext).as_slice().to_vec())
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .filter_map(|index| text.get(index..index.saturating_add(2)))
            .filter_map(|pair| u8::from_str_radix(pair, 16).ok())
            .collect()
    }

    /// RFC 9180 A.1.1 (DHKEM(X25519, HKDF-SHA256), HKDF-SHA256, AES-128-GCM, base mode),
    /// sequence number 0: the push relay suite, with `ikmE` fed through the tape.
    #[test]
    fn push_suite_matches_rfc9180_a_1_1() -> Result<(), CryptoError> {
        let ikm_e = hex("7268600d403fce431561aef583ee1613527cff655c1343f29812e66706df3234");
        let ikm_r: [u8; 32] = prefix32(&hex(
            "6db9df30aa07dd42ee5e8181afdb977e538f5e1fec8a06223f33f7013e525037",
        ))?;
        let info = hex("4f6465206f6e2061204772656369616e2055726e");
        let plaintext = hex("4265617574792069732074727574682c20747275746820626561757479");
        let aad_seq_0 = hex("436f756e742d30");
        let expected_enc = hex("37fda3567bdbd628e88668c3c8d7e97d1d1253b6d4ea6d44c150f741f1bf4431");
        let expected_ciphertext = hex(
            "f938558b5d72f1a23810b4be2ab4f84331acc02fc97babc53a52ae8218a355a96d8770ac83d07bea87e13c512a",
        );

        let recipient = derive_key_pair(Suite::PushRelay, &ikm_r)?;

        // The RFC's vector has AAD; Pingers seals with empty AAD, so call hpke-rs directly
        // here to check the tape and provider against the published bytes.
        let mut hpke = Suite::PushRelay.hpke(Some(Zeroizing::new(ikm_e)));
        let (enc, ciphertext) = hpke
            .seal(
                &HpkePublicKey::new(recipient.public.clone()),
                &info,
                &aad_seq_0,
                &plaintext,
                None,
                None,
                None,
            )
            .map_err(|_error| CryptoError::Internal)?;
        assert_eq!(enc, expected_enc);
        assert_eq!(ciphertext, expected_ciphertext);
        Ok(())
    }

    #[test]
    fn seal_fails_when_hpke_wants_more_randomness_than_drawn() {
        let result = seal_with_randomness(
            Recipient {
                suite: Suite::Grant,
                public_key: &[9; 32],
            },
            b"info",
            b"plaintext",
            Zeroizing::new(vec![0; 31]),
        );
        assert!(matches!(result, Err(CryptoError::Internal)));
    }

    #[test]
    fn low_order_public_key_is_invalid() {
        // The all-zero point has order 1; X25519 with it gives the all-zero shared secret.
        let result = seal_with_randomness(
            Recipient {
                suite: Suite::Grant,
                public_key: &[0; 32],
            },
            b"info",
            b"plaintext",
            Zeroizing::new(vec![1; 32]),
        );
        assert!(matches!(result, Err(CryptoError::InvalidKey)));
    }

    #[test]
    fn tape_replays_its_bytes_in_order_then_fails() {
        use rand_core::TryRng as _;
        let mut tape = Tape {
            bytes: Zeroizing::new((1..=12).collect()),
            position: 0,
        };
        assert_eq!(
            tape.try_next_u32().ok(),
            Some(u32::from_le_bytes([1, 2, 3, 4]))
        );
        assert_eq!(
            tape.try_next_u64().ok(),
            Some(u64::from_le_bytes([5, 6, 7, 8, 9, 10, 11, 12]))
        );
        assert!(tape.try_fill_bytes(&mut [0]).is_err());
        assert!(tape.try_next_u32().is_err());
        assert!(tape.try_next_u64().is_err());
    }

    #[test]
    fn provider_never_draws_on_its_own_prng() {
        // hpke-rs falls back to these when derandomized operations are unsupported; they must
        // fail rather than reach for randomness the caller did not supply.
        let mut tape = Tape {
            bytes: Zeroizing::new(vec![0; 64]),
            position: 0,
        };
        assert!(matches!(
            Provider::kem_key_gen(KemAlgorithm::DhKem25519, &mut tape),
            Err(ProviderError::UnsupportedKemOperation)
        ));
        assert!(matches!(
            Provider::kem_encaps(KemAlgorithm::DhKem25519, &[9; 32], &mut tape),
            Err(ProviderError::UnsupportedKemOperation)
        ));
        assert_eq!(tape.position, 0);
    }

    #[test]
    fn provider_refuses_primitives_of_other_suites() {
        assert!(Provider::kdf_derive(SingleStageKdfAlgorithm::Shake256, b"ikm", 32).is_err());
        assert!(Provider::dh_validate_sk(KemAlgorithm::DhKemP256, &[1; 32]).is_err());
        assert!(matches!(
            Provider::kem_key_gen_derand(KemAlgorithm::DhKem25519, &[1; 32]),
            Err(ProviderError::UnsupportedKemOperation)
        ));
        assert!(matches!(
            Provider::kem_encaps_derand(KemAlgorithm::DhKem25519, &[9; 32], &[1; 64]),
            Err(ProviderError::UnsupportedKemOperation)
        ));
        assert!(matches!(
            Provider::kem_decaps(KemAlgorithm::DhKem25519, &[9; 32], &[1; 32]),
            Err(ProviderError::UnsupportedKemOperation)
        ));
    }

    #[test]
    fn provider_supports_only_the_pinned_algorithms() {
        assert!(Provider::supports_kdf(KdfAlgorithm::HkdfSha256).is_ok());
        assert!(Provider::supports_kdf(KdfAlgorithm::HkdfSha384).is_err());
        assert!(Provider::supports_kem(KemAlgorithm::DhKem25519).is_ok());
        assert!(Provider::supports_kem(KemAlgorithm::DhKemP256).is_err());
        assert_eq!(
            Provider::supports_kem(KemAlgorithm::XWingDraft06).is_ok(),
            cfg!(feature = "x-wing")
        );
        assert!(Provider::supports_aead(AeadAlgorithm::Aes128Gcm).is_ok());
        assert!(Provider::supports_aead(AeadAlgorithm::ChaCha20Poly1305).is_ok());
        assert!(Provider::supports_aead(AeadAlgorithm::Aes256Gcm).is_err());
    }

    fn prefix32(bytes: &[u8]) -> Result<[u8; 32], CryptoError> {
        crate::primitives::prefix::<32>(bytes)
    }
}
