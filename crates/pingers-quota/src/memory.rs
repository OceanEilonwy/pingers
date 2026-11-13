// SPDX-License-Identifier: AGPL-3.0-only
//! An in-memory store for both quota traits, for tests and for servers that keep nothing on
//! disk.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, MutexGuard};

use pingers_anon::IssuerKey;
use pingers_proto::{AccountId, HourlyRoot};
use pingers_time::UnixSeconds;
use zeroize::Zeroizing;

use crate::QuotaError;
use crate::store::{
    Allowance, Issuance, IssuanceDay, IssuerKeyRecord, MembershipStore, PrivacyPassStore, Spend,
    StoredLeaf, TokenNonce, Zeroing,
};

/// Keeps everything behind one lock.
#[derive(Default)]
pub struct MemoryQuotaStore {
    inner: Mutex<Inner>,
}

/// A stored key: its secret bytes and validity.
struct KeyRow {
    secret: Zeroizing<Vec<u8>>,
    not_before: UnixSeconds,
    issue_until: UnixSeconds,
    redeem_until: UnixSeconds,
}

#[derive(Default)]
struct Inner {
    leaves: Vec<StoredLeaf>,
    roots: Vec<HourlyRoot>,
    keys: Vec<KeyRow>,
    spent: HashMap<TokenNonce, UnixSeconds>,
    issued: BTreeMap<(AccountId, IssuanceDay), u16>,
}

impl MemoryQuotaStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> Result<MutexGuard<'_, Inner>, QuotaError> {
        self.inner.lock().map_err(|_| QuotaError::Poisoned)
    }
}

impl Inner {
    fn zero(&mut self, zeroing: Zeroing) -> Result<(), QuotaError> {
        let leaf = self
            .leaves
            .iter_mut()
            .find(|leaf| leaf.index == zeroing.index)
            .ok_or(QuotaError::CorruptField { field: "rln_leaf" })?;
        leaf.zeroed_from = Some(zeroing.from);
        Ok(())
    }
}

impl MembershipStore for MemoryQuotaStore {
    fn leaves(&self) -> Result<Vec<StoredLeaf>, QuotaError> {
        Ok(self.lock()?.leaves.clone())
    }

    fn register(&self, leaf: &StoredLeaf, replaces: Option<Zeroing>) -> Result<(), QuotaError> {
        let mut inner = self.lock()?;
        if inner.leaves.iter().any(|stored| stored.index == leaf.index) {
            return Err(QuotaError::CorruptField { field: "rln_leaf" });
        }
        if let Some(zeroing) = replaces {
            inner.zero(zeroing)?;
        }
        inner.leaves.push(*leaf);
        Ok(())
    }

    fn zero(&self, zeroing: Zeroing) -> Result<(), QuotaError> {
        self.lock()?.zero(zeroing)
    }

    fn recent_roots(&self) -> Result<Vec<HourlyRoot>, QuotaError> {
        let inner = self.lock()?;
        let skip = inner.roots.len().saturating_sub(2);
        Ok(inner.roots.iter().skip(skip).copied().collect())
    }

    fn insert_root(&self, root: HourlyRoot) -> Result<(), QuotaError> {
        let mut inner = self.lock()?;
        if inner.roots.iter().any(|stored| stored.epoch == root.epoch) {
            return Err(QuotaError::CorruptField { field: "rln_root" });
        }
        inner.roots.push(root);
        Ok(())
    }
}

impl PrivacyPassStore for MemoryQuotaStore {
    fn keys(&self) -> Result<Vec<IssuerKeyRecord>, QuotaError> {
        self.lock()?
            .keys
            .iter()
            .map(|row| {
                Ok(IssuerKeyRecord {
                    key: IssuerKey::from_bytes(row.secret.clone())?,
                    not_before: row.not_before,
                    issue_until: row.issue_until,
                    redeem_until: row.redeem_until,
                })
            })
            .collect()
    }

    fn insert_key(&self, key: &IssuerKeyRecord) -> Result<(), QuotaError> {
        self.lock()?.keys.push(KeyRow {
            secret: Zeroizing::new(key.key.expose_bytes().to_vec()),
            not_before: key.not_before,
            issue_until: key.issue_until,
            redeem_until: key.redeem_until,
        });
        Ok(())
    }

    fn record_issuance(&self, issuance: Issuance, limit: u16) -> Result<Allowance, QuotaError> {
        let mut inner = self.lock()?;
        let issued = inner
            .issued
            .entry((issuance.account, issuance.day))
            .or_insert(0);
        match issued.checked_add(issuance.tokens) {
            Some(total) if total <= limit => {
                *issued = total;
                Ok(Allowance::Granted)
            }
            Some(_) | None => Ok(Allowance::Exceeded),
        }
    }

    fn spend(&self, nonce: TokenNonce, expires_at: UnixSeconds) -> Result<Spend, QuotaError> {
        let mut inner = self.lock()?;
        if inner.spent.contains_key(&nonce) {
            return Ok(Spend::AlreadySpent);
        }
        inner.spent.insert(nonce, expires_at);
        Ok(Spend::Fresh)
    }

    fn prune(&self, now: UnixSeconds) -> Result<(), QuotaError> {
        let mut inner = self.lock()?;
        inner.keys.retain(|row| row.redeem_until > now);
        inner.spent.retain(|_, expires_at| *expires_at > now);
        let today = IssuanceDay::containing(now);
        inner.issued.retain(|(_, day), _| *day >= today);
        Ok(())
    }
}
