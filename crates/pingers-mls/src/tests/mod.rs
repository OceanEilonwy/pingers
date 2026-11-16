// SPDX-License-Identifier: AGPL-3.0-only
//! Multi-member scenarios over the in-memory storage.

#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

mod meetup;
mod party;
mod vectors;
#[cfg(feature = "x-wing")]
mod x_wing;

use pingers_crypto::{AccountPublicKey, AccountSigningKey, GrantPublicKey, MeetupPsk};
use pingers_proto::{
    Epoch, GroupId, KeyPackageBytes, LeafIndex, MlsMessageBytes, Padded, PartyId, ProtoError,
    SizeClass, SpotId,
};
use pingers_time::Bucket;
use rand_chacha::ChaCha20Rng;
use zeroize::Zeroizing;

use crate::{
    CipherSuitePolicy, KeyPackagePurpose, MeetupPskId, MemoryStorage, MlsContext, MlsError,
    ProcessedMessage, Roster, RosterEntry,
};

/// Why a test failed before reaching an assertion.
#[derive(Debug, thiserror::Error)]
pub(crate) enum TestError {
    #[error(transparent)]
    Mls(#[from] MlsError),
    #[error(transparent)]
    Proto(#[from] ProtoError),
    #[error(transparent)]
    Vector(#[from] pingers_testkit::VectorError),
    #[error(transparent)]
    Time(#[from] pingers_time::TimeError),
    #[error("expected {0}")]
    Missing(&'static str),
}

pub(crate) type TestResult = Result<(), TestError>;

/// One device: its account key and its own storage.
pub(crate) struct Device {
    pub(crate) key: AccountSigningKey,
    pub(crate) storage: MemoryStorage,
}

impl Device {
    pub(crate) fn new(rng: &mut ChaCha20Rng) -> Self {
        Self {
            key: AccountSigningKey::generate(rng),
            storage: MemoryStorage::new(),
        }
    }

    pub(crate) fn context(&self) -> MlsContext<'_, MemoryStorage> {
        MlsContext::new(&self.storage, &self.key)
    }

    pub(crate) fn public_key(&self) -> AccountPublicKey {
        self.key.public_key()
    }

    pub(crate) fn party_key_package(
        &self,
        rng: &mut ChaCha20Rng,
    ) -> Result<KeyPackageBytes, MlsError> {
        self.context().create_key_package(
            KeyPackagePurpose::Party,
            CipherSuitePolicy::Classical,
            rng,
        )
    }

    pub(crate) fn meetup_key_package(
        &self,
        grant_key: &GrantPublicKey,
        rng: &mut ChaCha20Rng,
    ) -> Result<KeyPackageBytes, MlsError> {
        self.meetup_key_package_with(grant_key, None, rng)
    }

    pub(crate) fn meetup_key_package_with(
        &self,
        grant_key: &GrantPublicKey,
        subscription: Option<&pingers_proto::SubscriptionId>,
        rng: &mut ChaCha20Rng,
    ) -> Result<KeyPackageBytes, MlsError> {
        self.context().create_key_package(
            KeyPackagePurpose::Meetup {
                grant_key,
                subscription,
            },
            CipherSuitePolicy::Classical,
            rng,
        )
    }
}

pub(crate) fn rng(seed: u64) -> ChaCha20Rng {
    pingers_testkit::seeded_rng(seed)
}

/// A Party roster naming these devices at leaves 0, 1, ...
pub(crate) fn roster_of(devices: &[&Device]) -> Roster {
    Roster::new(
        devices
            .iter()
            .zip(0..)
            .map(|(device, leaf)| RosterEntry {
                leaf: LeafIndex::new(leaf),
                signature_key: device.public_key(),
            })
            .collect(),
    )
}

pub(crate) fn group_id(byte: u8) -> GroupId {
    GroupId::from_bytes([byte; 32])
}

pub(crate) fn padded(text: &[u8]) -> Result<Padded, ProtoError> {
    Padded::pad(text, SizeClass::Bytes256)
}

pub(crate) fn grant_key(byte: u8) -> GrantPublicKey {
    GrantPublicKey::from_bytes(vec![byte; 32])
}

pub(crate) fn psk_id(epoch: u64) -> Result<MeetupPskId, TestError> {
    Ok(MeetupPskId {
        party: PartyId::from_bytes([0x11; 16]),
        epoch: Epoch::new(epoch),
        spot: SpotId::from_bytes([0x22; 16]),
        bucket: Bucket::from_index(1_000)?,
    })
}

pub(crate) fn meetup_psk(byte: u8) -> MeetupPsk {
    MeetupPsk::from_bytes(Zeroizing::new([byte; 32]))
}

/// The plaintext of an application message, or a test failure.
pub(crate) fn application(processed: ProcessedMessage) -> Result<(LeafIndex, Vec<u8>), TestError> {
    match processed {
        ProcessedMessage::Application { sender, plaintext } => Ok((sender, plaintext.to_vec())),
        ProcessedMessage::Commit(_) | ProcessedMessage::Proposal { .. } => {
            Err(TestError::Missing("an application message"))
        }
    }
}

/// The summary of a processed commit, or a test failure.
pub(crate) fn commit_summary(
    processed: ProcessedMessage,
) -> Result<crate::CommitSummary, TestError> {
    match processed {
        ProcessedMessage::Commit(summary) => Ok(summary),
        ProcessedMessage::Application { .. } | ProcessedMessage::Proposal { .. } => {
            Err(TestError::Missing("a commit"))
        }
    }
}

/// `message` with one byte of its ciphertext flipped.
pub(crate) fn tampered(message: &MlsMessageBytes) -> MlsMessageBytes {
    let mut bytes = message.as_bytes().to_vec();
    if let Some(last) = bytes.last_mut() {
        *last ^= 0x01;
    }
    MlsMessageBytes::from_bytes(bytes)
}
