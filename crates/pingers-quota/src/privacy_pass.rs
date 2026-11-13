// SPDX-License-Identifier: AGPL-3.0-only
//! The Privacy Pass issuer (design B.4.8, C.1, D): weekly keys, a daily allowance per account,
//! and the spent set that makes tokens single-use.

use core::time::Duration;
use std::sync::{Arc, Mutex, MutexGuard};

use pingers_anon::{AnonError, IssuerKey, check_redemption, issue_tokens};
use pingers_proto::AccountId;
use pingers_time::{Clock, UnixSeconds};
use rand_core::CryptoRng;

use crate::QuotaError;
use crate::store::{
    Allowance, Issuance, IssuanceDay, IssuerKeyRecord, PrivacyPassStore, Spend, TokenNonce,
};

/// How long one key issues tokens before the next takes over (design C.1: rotated weekly).
pub const ISSUANCE_PERIOD: Duration = Duration::from_hours(7 * 24);

/// How long after a key stops issuing its tokens are still accepted, so a token fetched on
/// the key's last day is still good for a week.
pub const REDEMPTION_GRACE: Duration = Duration::from_hours(7 * 24);

/// Tokens one account may be issued per UTC day (design B.4.8: a batch of up to 300 a day).
pub const DAILY_TOKEN_LIMIT: u16 = 300;

/// A public key to publish at `GET /v1/pp/keys`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssuerPublicKey {
    /// `SHA-256(public_key)`.
    pub key_id: [u8; 32],
    /// The compressed P-384 point.
    pub public_key: Vec<u8>,
    /// Until when it issues; fetch the next key after this.
    pub issue_until: UnixSeconds,
    /// Until when its tokens are accepted.
    pub redeem_until: UnixSeconds,
}

/// The answer to `POST /v1/pp/issue`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issued {
    /// Which key evaluated the batch.
    pub key_id: [u8; 32],
    /// The batched issuance response (evaluated elements and the DLEQ proof).
    pub response: Vec<u8>,
}

/// The issuer.
pub struct PrivacyPass<S> {
    store: S,
    clock: Arc<dyn Clock>,
    keys: Mutex<Vec<IssuerKeyRecord>>,
}

impl<S: PrivacyPassStore> PrivacyPass<S> {
    /// Loads the stored keys.
    ///
    /// # Errors
    ///
    /// Storage errors; [`QuotaError::Anon`] if a stored key does not load.
    pub fn new(store: S, clock: Arc<dyn Clock>) -> Result<Self, QuotaError> {
        let keys = store.keys()?;
        Ok(Self {
            store,
            clock,
            keys: Mutex::new(keys),
        })
    }

    fn lock(&self) -> Result<MutexGuard<'_, Vec<IssuerKeyRecord>>, QuotaError> {
        self.keys.lock().map_err(|_| QuotaError::Poisoned)
    }

    /// Forgets expired keys, spent nonces and old issuance counts, and starts a new key if none
    /// is issuing now. Called before every issuance; call it daily as well.
    ///
    /// # Errors
    ///
    /// Storage errors; [`QuotaError::Anon`] if key generation fails;
    /// [`QuotaError::TimeOutOfRange`].
    pub fn rotate(&self, rng: &mut impl CryptoRng) -> Result<(), QuotaError> {
        let now = self.clock.now();
        let mut keys = self.lock()?;
        self.rotate_locked(&mut keys, now, rng)
    }

    fn rotate_locked(
        &self,
        keys: &mut Vec<IssuerKeyRecord>,
        now: UnixSeconds,
        rng: &mut impl CryptoRng,
    ) -> Result<(), QuotaError> {
        self.store.prune(now)?;
        keys.retain(|record| record.redeem_until > now);
        if keys.iter().any(|record| issues_at(record, now)) {
            return Ok(());
        }
        let issue_until = now
            .checked_add(ISSUANCE_PERIOD)
            .ok_or(QuotaError::TimeOutOfRange)?;
        let record = IssuerKeyRecord {
            key: IssuerKey::generate(rng)?,
            not_before: now,
            issue_until,
            redeem_until: issue_until
                .checked_add(REDEMPTION_GRACE)
                .ok_or(QuotaError::TimeOutOfRange)?,
        };
        self.store.insert_key(&record)?;
        keys.push(record);
        Ok(())
    }

    /// `GET /v1/pp/keys`: every key whose tokens are still accepted, oldest first.
    ///
    /// # Errors
    ///
    /// [`QuotaError::Poisoned`].
    pub fn public_keys(&self) -> Result<Vec<IssuerPublicKey>, QuotaError> {
        let now = self.clock.now();
        Ok(self
            .lock()?
            .iter()
            .filter(|record| record.redeem_until > now)
            .map(|record| IssuerPublicKey {
                key_id: record.key.key_id(),
                public_key: record.key.public_key(),
                issue_until: record.issue_until,
                redeem_until: record.redeem_until,
            })
            .collect())
    }

    /// `POST /v1/pp/issue` (Acct): evaluates a batch with the current key and counts it
    /// against the account's daily allowance. Nothing is returned or counted if the batch
    /// would exceed it.
    ///
    /// # Errors
    ///
    /// [`QuotaError::DailyLimit`]; [`QuotaError::Anon`] with [`AnonError::BadIssuance`] if
    /// the batch is malformed or made for another key; storage errors.
    pub fn issue(
        &self,
        account: AccountId,
        request: &[u8],
        rng: &mut impl CryptoRng,
    ) -> Result<Issued, QuotaError> {
        let now = self.clock.now();
        let mut keys = self.lock()?;
        self.rotate_locked(&mut keys, now, rng)?;
        let record = keys
            .iter()
            .rev()
            .find(|record| issues_at(record, now))
            .ok_or(QuotaError::Anon(AnonError::Internal))?;
        let response = issue_tokens(&record.key, request, rng)?;
        let issuance = Issuance {
            account,
            day: IssuanceDay::containing(now),
            tokens: batch_size(request)?,
        };
        match self.store.record_issuance(issuance, DAILY_TOKEN_LIMIT)? {
            Allowance::Granted => Ok(Issued {
                key_id: record.key.key_id(),
                response,
            }),
            Allowance::Exceeded => Err(QuotaError::DailyLimit),
        }
    }

    /// Redeems an `Authorization: PrivateToken` token: it must verify under a key whose tokens
    /// are still accepted, and its nonce must not be in the spent set.
    ///
    /// # Errors
    ///
    /// [`QuotaError::Anon`] with [`AnonError::BadToken`] if no accepted key verifies it;
    /// [`QuotaError::DoubleSpend`]; storage errors.
    pub fn redeem(&self, token: &[u8]) -> Result<(), QuotaError> {
        let now = self.clock.now();
        let keys = self.lock()?;
        let verified = keys
            .iter()
            .filter(|record| record.redeem_until > now)
            .find_map(|record| {
                check_redemption(&record.key, token)
                    .ok()
                    .map(|nonce| (TokenNonce::from_bytes(nonce), record.redeem_until))
            });
        let (nonce, expires_at) = verified.ok_or(QuotaError::Anon(AnonError::BadToken))?;
        match self.store.spend(nonce, expires_at)? {
            Spend::Fresh => Ok(()),
            Spend::AlreadySpent => Err(QuotaError::DoubleSpend),
        }
    }
}

fn issues_at(record: &IssuerKeyRecord, now: UnixSeconds) -> bool {
    record.not_before <= now && now < record.issue_until
}

/// The number of tokens in a batched request that `issue_tokens` accepted:
/// `token_type(2) || truncated_key_id(1) || length(2) || 49-byte elements` (RFC 9578, as
/// pingers-anon encodes it).
fn batch_size(request: &[u8]) -> Result<u16, QuotaError> {
    const HEADER: usize = 5;
    const ELEMENT: usize = 49;
    let elements = request
        .len()
        .checked_sub(HEADER)
        .and_then(|body| body.checked_div(ELEMENT))
        .ok_or(QuotaError::Anon(AnonError::BadIssuance))?;
    u16::try_from(elements).map_err(|_| QuotaError::Anon(AnonError::BadIssuance))
}
