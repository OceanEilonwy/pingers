// SPDX-License-Identifier: AGPL-3.0-only
//! The storage traits the quota service owns (design C.1: `account`, `account_bearer`,
//! `rln_leaf`, `rln_root`, `pp_key`, `pp_spent`, `pp_quota`), and the rows they hold.
//!
//! The nullifier set has no trait: design C.1 keeps it in memory only.

use std::sync::Arc;

use pingers_anon::{IssuerKey, LeafValue};
use pingers_crypto::{AccountPublicKey, BearerDigest};
use pingers_proto::{AccountId, HourlyRoot, MembershipIndex, RootEpoch};
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
    fn recent_roots(&self) -> Result<Vec<HourlyRoot>, QuotaError>;

    /// Records a new hourly root.
    ///
    /// # Errors
    ///
    /// Storage errors, including an epoch that already has a root.
    fn insert_root(&self, root: HourlyRoot) -> Result<(), QuotaError>;
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

    /// When the next day starts, or `None` past the end of time.
    #[must_use]
    pub const fn next(self) -> Option<UnixSeconds> {
        match self.0.checked_add(1) {
            Some(day) => match day.checked_mul(Self::SECONDS) {
                Some(seconds) => Some(UnixSeconds::new(seconds)),
                None => None,
            },
            None => None,
        }
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
    Exceeded {
        /// How many tokens the account had already been issued that day.
        issued: u16,
    },
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

    fn recent_roots(&self) -> Result<Vec<HourlyRoot>, QuotaError> {
        (**self).recent_roots()
    }

    fn insert_root(&self, root: HourlyRoot) -> Result<(), QuotaError> {
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

/// A new account (design C.1 `account`): only its public key and when it was made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NewAccount {
    /// `SHA-256(pk)[..16]`.
    pub id: AccountId,
    /// The Ed25519 key that signs its auth challenges.
    pub key: AccountPublicKey,
    /// When it registered, for the sign-up limit.
    pub created_at: UnixSeconds,
}

/// At most `max` accounts may have been created at or after `since` (the sign-up limit).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SignupWindow {
    /// The start of the window.
    pub since: UnixSeconds,
    /// How many accounts the window allows.
    pub max: u32,
}

/// What [`AccountStore::insert_account`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub enum AccountInsert {
    /// The account is new and was stored.
    Created,
    /// The account was already stored; nothing changed.
    Existing,
    /// The window is full; nothing was stored.
    WindowFull,
}

/// A bearer as stored: its digest, never the bearer itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoredBearer {
    /// `bearer_digest(bearer)`.
    pub digest: BearerDigest,
    /// Whose it is.
    pub account: AccountId,
    /// When it stops being accepted.
    pub expires_at: UnixSeconds,
}

/// Durable accounts and their bearers, so a restart neither forgets a device nor ends its
/// bearer early.
pub trait AccountStore: Send + Sync {
    /// Stores `account` unless it exists, and only if `window` still has room.
    ///
    /// # Errors
    ///
    /// Storage errors.
    fn insert_account(
        &self,
        account: &NewAccount,
        window: SignupWindow,
    ) -> Result<AccountInsert, QuotaError>;

    /// The key of a stored account.
    ///
    /// # Errors
    ///
    /// Storage errors.
    fn account_key(&self, account: &AccountId) -> Result<Option<AccountPublicKey>, QuotaError>;

    /// Deletes an account and every bearer of it. Deleting an unknown account does nothing.
    ///
    /// # Errors
    ///
    /// Storage errors.
    fn delete_account(&self, account: &AccountId) -> Result<(), QuotaError>;

    /// Stores a bearer and keeps at most `keep` of the account's bearers: the new one, and of
    /// the others those that expire last.
    ///
    /// # Errors
    ///
    /// Storage errors.
    fn insert_bearer(&self, bearer: &StoredBearer, keep: usize) -> Result<(), QuotaError>;

    /// The account a bearer digest belongs to, if the bearer is unexpired at `now` and the
    /// account still exists.
    ///
    /// # Errors
    ///
    /// Storage errors.
    fn bearer_account(
        &self,
        digest: &BearerDigest,
        now: UnixSeconds,
    ) -> Result<Option<AccountId>, QuotaError>;

    /// Forgets bearers expired at `now`.
    ///
    /// # Errors
    ///
    /// Storage errors.
    fn prune_bearers(&self, now: UnixSeconds) -> Result<(), QuotaError>;
}

impl<S: AccountStore + ?Sized> AccountStore for Arc<S> {
    fn insert_account(
        &self,
        account: &NewAccount,
        window: SignupWindow,
    ) -> Result<AccountInsert, QuotaError> {
        (**self).insert_account(account, window)
    }

    fn account_key(&self, account: &AccountId) -> Result<Option<AccountPublicKey>, QuotaError> {
        (**self).account_key(account)
    }

    fn delete_account(&self, account: &AccountId) -> Result<(), QuotaError> {
        (**self).delete_account(account)
    }

    fn insert_bearer(&self, bearer: &StoredBearer, keep: usize) -> Result<(), QuotaError> {
        (**self).insert_bearer(bearer, keep)
    }

    fn bearer_account(
        &self,
        digest: &BearerDigest,
        now: UnixSeconds,
    ) -> Result<Option<AccountId>, QuotaError> {
        (**self).bearer_account(digest, now)
    }

    fn prune_bearers(&self, now: UnixSeconds) -> Result<(), QuotaError> {
        (**self).prune_bearers(now)
    }
}
