// SPDX-License-Identifier: AGPL-3.0-only
//! The OpenMLS provider for one operation: crypto, injected randomness and the storage adapter.

use core::cell::RefCell;

use openmls::prelude::{BasicCredential, CredentialWithKey, SignatureScheme};
use openmls_basic_credential::SignatureKeyPair;
use openmls_traits::OpenMlsProvider;
use openmls_traits::random::OpenMlsRand;
use pingers_crypto::AccountSigningKey;
use rand_core::CryptoRng;

use crate::adapter::StorageAdapter;
use crate::{MlsError, MlsStorage};

/// The OpenMLS crypto backend: `RustCrypto`, or libcrux when the X-Wing suite is built in (only
/// libcrux implements it).
#[cfg(not(feature = "x-wing"))]
pub(crate) type Crypto = openmls_rust_crypto::RustCrypto;
/// The OpenMLS crypto backend: `RustCrypto`, or libcrux when the X-Wing suite is built in (only
/// libcrux implements it).
#[cfg(feature = "x-wing")]
pub(crate) type Crypto = openmls_libcrux_crypto::CryptoProvider;

/// A crypto backend instance.
#[cfg(not(feature = "x-wing"))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the libcrux backend's constructor is fallible"
)]
pub(crate) fn new_crypto() -> Result<Crypto, MlsError> {
    Ok(Crypto::default())
}

/// A crypto backend instance.
#[cfg(feature = "x-wing")]
pub(crate) fn new_crypto() -> Result<Crypto, MlsError> {
    Crypto::new().map_err(|_| MlsError::Internal)
}

/// Crypto, randomness and storage for one wrapper call.
pub(crate) struct Provider<'storage, S: MlsStorage, R> {
    crypto: Crypto,
    rand: R,
    storage: StorageAdapter<'storage, S>,
}

impl<'storage, S: MlsStorage, R: OpenMlsRand> Provider<'storage, S, R> {
    pub(crate) fn new(storage: &'storage S, rand: R) -> Result<Self, MlsError> {
        Ok(Self {
            crypto: new_crypto()?,
            rand,
            storage: StorageAdapter::new(storage),
        })
    }

    /// The storage adapter, to turn an OpenMLS failure into the right [`crate::MlsError`].
    pub(crate) const fn adapter(&self) -> &StorageAdapter<'storage, S> {
        &self.storage
    }
}

impl<'storage, S: MlsStorage, R: OpenMlsRand> OpenMlsProvider for Provider<'storage, S, R> {
    type CryptoProvider = Crypto;
    type RandProvider = R;
    type StorageProvider = StorageAdapter<'storage, S>;

    fn storage(&self) -> &Self::StorageProvider {
        &self.storage
    }

    fn crypto(&self) -> &Self::CryptoProvider {
        &self.crypto
    }

    fn rand(&self) -> &Self::RandProvider {
        &self.rand
    }
}

/// Why OpenMLS could not have randomness.
#[derive(Debug, thiserror::Error)]
#[error("randomness unavailable")]
pub(crate) struct RandUnavailable;

/// OpenMLS randomness drawn from the caller's RNG (style guide §6).
pub(crate) struct InjectedRand<'rng, R: CryptoRng> {
    rng: RefCell<&'rng mut R>,
}

impl<'rng, R: CryptoRng> InjectedRand<'rng, R> {
    pub(crate) const fn new(rng: &'rng mut R) -> Self {
        Self {
            rng: RefCell::new(rng),
        }
    }
}

impl<R: CryptoRng> OpenMlsRand for InjectedRand<'_, R> {
    type Error = RandUnavailable;

    fn random_array<const N: usize>(&self) -> Result<[u8; N], Self::Error> {
        let mut bytes = [0; N];
        self.rng
            .try_borrow_mut()
            .map_err(|_| RandUnavailable)?
            .fill_bytes(&mut bytes);
        Ok(bytes)
    }

    fn random_vec(&self, length: usize) -> Result<Vec<u8>, Self::Error> {
        let mut bytes = vec![0; length];
        self.rng
            .try_borrow_mut()
            .map_err(|_| RandUnavailable)?
            .fill_bytes(&mut bytes);
        Ok(bytes)
    }
}

/// Randomness for operations that the contract gives no RNG (processing, joining, merging):
/// OpenMLS needs none for them, and if it ever asks, the call fails rather than using an
/// uninjected source.
pub(crate) struct NoRand;

impl OpenMlsRand for NoRand {
    type Error = RandUnavailable;

    fn random_array<const N: usize>(&self) -> Result<[u8; N], Self::Error> {
        Err(RandUnavailable)
    }

    fn random_vec(&self, _length: usize) -> Result<Vec<u8>, Self::Error> {
        Err(RandUnavailable)
    }
}

/// The OpenMLS signer for the account key, which is the leaf signature key (design B.1).
/// Signing itself happens in `openmls_basic_credential`.
pub(crate) fn leaf_signer(key: &AccountSigningKey) -> SignatureKeyPair {
    SignatureKeyPair::from_raw(
        SignatureScheme::ED25519,
        key.expose_bytes().to_vec(),
        key.public_key().as_bytes().to_vec(),
    )
}

/// The leaf credential: a `BasicCredential` whose identity is the account public key, with
/// the same key as signature key.
pub(crate) fn credential(key: &AccountSigningKey) -> CredentialWithKey {
    let public = key.public_key().as_bytes().to_vec();
    CredentialWithKey {
        credential: BasicCredential::new(public.clone()).into(),
        signature_key: public.into(),
    }
}
