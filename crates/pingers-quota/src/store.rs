// SPDX-License-Identifier: AGPL-3.0-only
//! The storage traits the quota service owns (design C.1: `rln_leaf`, `rln_root`, `pp_key`,
//! `pp_spent`, `pp_quota`), and the rows they hold.
//!
//! The nullifier set has no trait: design C.1 keeps it in memory only.

use std::sync::Arc;

use pingers_anon::{IssuerKey, LeafValue};
use pingers_proto::{AccountId, MembershipIndex, MembershipRoot, RootEpoch};
use pingers_time::UnixSeconds;

use crate::QuotaError;

/// One leaf of the membership tree as stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoredLeaf {
    /// Its position.
    pub index: MembershipIndex,
    /// `rate_commitment = Poseidon(id_commitment, 8)`.
    pub value: LeafValue,
    /// The account that registered it, so that a deleted or banned account's leaf can be
    /// zeroed. It does not let the server link proofs to leaves.
    pub account: AccountId,
    /// The first root epoch whose root contains it.
    pub live_from: RootEpoch,
    /// The first root epoch whose root has it zeroed, if it is being removed.
    pub zeroed_from: Option<RootEpoch>,
}

/// A leaf being zeroed from a root epoch on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Zeroing {
    /// Which leaf.
    pub index: MembershipIndex,
    /// From which root epoch.
    pub from: RootEpoch,
}

/// Durable state of the membership tree. The service holds the tree in memory and writes
/// through; on start it rebuilds the tree from [`MembershipStore::leaves`] and checks it
/// against the latest stored root.
pub trait MembershipStore: Send + Sync {
    /// Every leaf, in index order.
    ///
    /// # Errors
    ///
    /// Storage errors.
    fn leaves(&self) -> Result<Vec<StoredLeaf>, QuotaError>;

    /// Adds a leaf and, in the same transaction, zeroes the leaf it replaces (an account
    /// that registers again).
    ///
    /// # Errors
    ///
    /// Storage errors, including a taken index.
    fn register(&self, leaf: &StoredLeaf, replaces: Option<Zeroing>) -> Result<(), QuotaError>;

    /// Marks a leaf as zeroed from a root epoch on.
    ///
    /// # Errors
    ///
    /// Storage errors.
    fn zero(&self, zeroing: Zeroing) -> Result<(), QuotaError>;

    /// The two latest hourly roots, oldest first.
    ///
    /// # Errors
    ///
    /// Storage errors.
    fn recent_roots(&self) -> Result<Vec<MembershipRoot>, QuotaError>;

    /// Records a new hourly root.
    ///
    /// # Errors
    ///
    /// Storage errors, including an epoch that already has a root.
    fn insert_root(&self, root: MembershipRoot) -> Result<(), QuotaError>;
}

/// A Privacy Pass issuer key with its validity (design C.1 `pp_key`; rotated weekly).
/// No `Debug` or `Clone`: it holds the private key.
pub struct IssuerKeyRecord {
    /// The key.
    pub key: IssuerKey,
    /// When it started issuing.
    pub not_before: UnixSeconds,
    /// When it stops issuing; the next key takes over.
    pub issue_until: UnixSeconds,
    /// When its tokens stop being accepted, and its spent nonces may be forgotten.
    pub redeem_until: UnixSeconds,
}

/// The nonce of a redeemed token, which the spent set is keyed by.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TokenNonce([u8; 32]);

impl TokenNonce {
    /// Wraps a nonce from `check_redemption` or the store.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The nonce.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl core::fmt::Debug for TokenNonce {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("TokenNonce(redacted)")
    }
}

/// A UTC day, the period of the issuance allowance (design C.1 `pp_quota.day`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IssuanceDay(u64);

impl IssuanceDay {
    const SECONDS: u64 = 24 * 60 * 60;

    /// The day containing `instant`.
    #[must_use]
    pub const fn containing(instant: UnixSeconds) -> Self {
        Self(instant.get() / Self::SECONDS)
    }

    /// Wraps a stored day number.
    #[must_use]
    pub const fn new(day: u64) -> Self {
        Self(day)
    }

    /// The day number (days since the Unix epoch).
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Tokens issued to an account, to count against its daily allowance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Issuance {
    /// Who asked.
    pub account: AccountId,
    /// Which day.
    pub day: IssuanceDay,
    /// How many tokens.
    pub tokens: u16,
}

/// Whether an issuance fit in the allowance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub enum Allowance {
    /// Counted.
    Granted,
    /// It would exceed the limit; nothing was counted.
    Exceeded,
}

/// Whether a nonce was new to the spent set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub enum Spend {
    /// First redemption; recorded.
    Fresh,
    /// Already recorded.
    AlreadySpent,
}

/// Durable state of the Privacy Pass issuer.
pub trait PrivacyPassStore: Send + Sync {
    /// Every stored key, oldest first.
    ///
    /// # Errors
    ///
    /// Storage errors; [`QuotaError::Anon`] if a stored key does not load.
    fn keys(&self) -> Result<Vec<IssuerKeyRecord>, QuotaError>;

    /// Stores a new key.
    ///
    /// # Errors
    ///
    /// Storage errors.
    fn insert_key(&self, key: &IssuerKeyRecord) -> Result<(), QuotaError>;

    /// Atomically adds `issuance.tokens` to the account's count for the day if the total stays
    /// within `limit`.
    ///
    /// # Errors
    ///
    /// Storage errors.
    fn record_issuance(&self, issuance: Issuance, limit: u16) -> Result<Allowance, QuotaError>;

    /// Atomically adds a nonce to the spent set unless it is there.
    ///
    /// # Errors
    ///
    /// Storage errors.
    fn spend(&self, nonce: TokenNonce, expires_at: UnixSeconds) -> Result<Spend, QuotaError>;

    /// Forgets keys whose redemption ended, spent nonces that expired, and issuance counts of
    /// days before `now`'s.
    ///
    /// # Errors
    ///
    /// Storage errors.
    fn prune(&self, now: UnixSeconds) -> Result<(), QuotaError>;
}

impl<S: MembershipStore + ?Sized> MembershipStore for Arc<S> {
    fn leaves(&self) -> Result<Vec<StoredLeaf>, QuotaError> {
        (**self).leaves()
    }

    fn register(&self, leaf: &StoredLeaf, replaces: Option<Zeroing>) -> Result<(), QuotaError> {
        (**self).register(leaf, replaces)
    }

    fn zero(&self, zeroing: Zeroing) -> Result<(), QuotaError> {
        (**self).zero(zeroing)
    }

    fn recent_roots(&self) -> Result<Vec<MembershipRoot>, QuotaError> {
        (**self).recent_roots()
    }

    fn insert_root(&self, root: MembershipRoot) -> Result<(), QuotaError> {
        (**self).insert_root(root)
    }
}

impl<S: PrivacyPassStore + ?Sized> PrivacyPassStore for Arc<S> {
    fn keys(&self) -> Result<Vec<IssuerKeyRecord>, QuotaError> {
        (**self).keys()
    }

    fn insert_key(&self, key: &IssuerKeyRecord) -> Result<(), QuotaError> {
        (**self).insert_key(key)
    }

    fn record_issuance(&self, issuance: Issuance, limit: u16) -> Result<Allowance, QuotaError> {
        (**self).record_issuance(issuance, limit)
    }

    fn spend(&self, nonce: TokenNonce, expires_at: UnixSeconds) -> Result<Spend, QuotaError> {
        (**self).spend(nonce, expires_at)
    }

    fn prune(&self, now: UnixSeconds) -> Result<(), QuotaError> {
        (**self).prune(now)
    }
}
