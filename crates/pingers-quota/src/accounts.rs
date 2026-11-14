// SPDX-License-Identifier: AGPL-3.0-only
//! Accounts and the Acct credential (design B.1, D): registration with a proof of key
//! possession (and an operator's registration code if configured), and a bearer for a signed
//! challenge, valid for an hour.
//!
//! Both persist in the [`AccountStore`], so a restart neither makes devices register again nor
//! ends their bearers early, and a registration code changed later never locks out a device
//! that registered under the old one: a known key is let in by its proof alone. The store keeps
//! only the public key, the time of registration and, per bearer, its digest
//! ([`pingers_crypto::bearer_digest`]); a copy of the database holds no usable bearer.
//!
//! Limits: at most [`AccountPolicy::signups_per_hour`] new accounts per hour (so an open
//! server cannot be flooded with accounts, each of which would take a membership leaf and a
//! token allowance), and at most [`BEARERS_PER_ACCOUNT`] live bearers per account.

use core::num::NonZeroU32;
use core::time::Duration;
use std::sync::Arc;

use pingers_crypto::{
    AccountPublicKey, BearerToken, Signature, account_id, bearer_digest, verify_auth_challenge,
    verify_registration,
};
use pingers_proto::{AccountId, AuthRequest, RegisterRequest, SecretText, ServerId};
use pingers_time::{Bucket, Clock, UnixSeconds};
use subtle::ConstantTimeEq as _;

use crate::QuotaError;
use crate::store::{AccountInsert, AccountStore, NewAccount, SignupWindow, StoredBearer};

/// How long a bearer is valid (design D: one hour).
pub const BEARER_LIFETIME: Duration = Duration::from_hours(1);

/// How long after a bucket rollover a challenge signed for the previous bucket still counts.
pub const CHALLENGE_GRACE: Duration = Duration::from_secs(60);

/// The most bearers one account holds at once; a new one replaces the one that expires first.
/// A device needs one; a few allow for a lost answer and the app and its extensions.
pub const BEARERS_PER_ACCOUNT: usize = 4;

/// The window the sign-up limit counts over.
pub const SIGNUP_WINDOW: Duration = Duration::from_hours(1);

/// How many new accounts the server takes per [`SIGNUP_WINDOW`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SignupsPerHour(NonZeroU32);

impl SignupsPerHour {
    /// Wraps a configured limit.
    #[must_use]
    pub const fn new(limit: NonZeroU32) -> Self {
        Self(limit)
    }

    /// The limit.
    #[must_use]
    pub const fn get(self) -> NonZeroU32 {
        self.0
    }
}

/// The operator's account policy.
pub struct AccountPolicy {
    /// This server's origin, which registration proofs and challenges commit to.
    pub server: ServerId,
    /// Codes that allow a new account; empty means anyone may register (design B.1).
    pub registration_codes: Vec<SecretText>,
    /// The sign-up limit.
    pub signups_per_hour: SignupsPerHour,
}

/// The accounts this server knows, and their bearers.
pub struct Accounts<S> {
    store: S,
    clock: Arc<dyn Clock>,
    policy: AccountPolicy,
}

impl<S: AccountStore> Accounts<S> {
    /// The accounts in `store` under `policy`, reading time from `clock`.
    pub fn new(store: S, clock: Arc<dyn Clock>, policy: AccountPolicy) -> Self {
        Self {
            store,
            clock,
            policy,
        }
    }

    fn code_accepted(&self, code: Option<&SecretText>) -> bool {
        if self.policy.registration_codes.is_empty() {
            return true;
        }
        let Some(code) = code else {
            return false;
        };
        // Every configured code is compared, so the time taken does not say which matched.
        self.policy
            .registration_codes
            .iter()
            .fold(subtle::Choice::from(0), |found, known| {
                found | known.ct_eq(code)
            })
            .into()
    }

    /// `POST /v1/accounts`. Registering a known key again is not an error, and needs no
    /// code: the proof shows the caller holds the key.
    ///
    /// # Errors
    ///
    /// [`QuotaError::BadSignature`] if the proof does not verify for this server;
    /// [`QuotaError::RegistrationCode`] for a new account without an accepted code;
    /// [`QuotaError::SignupLimit`] if the hour's new accounts are used up; storage errors.
    pub fn register(&self, request: &RegisterRequest) -> Result<AccountId, QuotaError> {
        let key = AccountPublicKey::from_bytes(request.signature_key);
        verify_registration(
            &key,
            &self.policy.server,
            &Signature::from_bytes(request.proof),
        )
        .map_err(|_| QuotaError::BadSignature)?;
        let id = account_id(&key);
        if self.store.account_key(&id)?.is_some() {
            return Ok(id);
        }
        if !self.code_accepted(request.registration_code.as_ref()) {
            return Err(QuotaError::RegistrationCode);
        }
        let now = self.clock.now();
        let window = SignupWindow {
            since: now.checked_sub(SIGNUP_WINDOW).unwrap_or(UnixSeconds::EPOCH),
            max: self.policy.signups_per_hour.get().get(),
        };
        let account = NewAccount {
            id,
            key,
            created_at: now,
        };
        match self.store.insert_account(&account, window)? {
            AccountInsert::Created | AccountInsert::Existing => Ok(id),
            AccountInsert::WindowFull => Err(QuotaError::SignupLimit),
        }
    }

    /// `POST /v1/auth`: stores `bearer` (fresh random bytes from the caller) for the account
    /// if its challenge for the current bucket, or the previous within [`CHALLENGE_GRACE`],
    /// verifies. Returns when the bearer expires.
    ///
    /// # Errors
    ///
    /// [`QuotaError::BadSignature`] for an unknown account or a challenge that does not
    /// verify; [`QuotaError::TimeOutOfRange`]; storage errors.
    pub fn authenticate(
        &self,
        request: &AuthRequest,
        bearer: &BearerToken,
    ) -> Result<UnixSeconds, QuotaError> {
        let key = self
            .store
            .account_key(&request.account_id)?
            .ok_or(QuotaError::BadSignature)?;
        let now = self.clock.now();
        let signature = Signature::from_bytes(request.nonce_signature);
        let current = now.bucket();
        let in_grace = now.saturating_duration_since(current.start()) < CHALLENGE_GRACE;
        let buckets: [Option<Bucket>; 2] = [Some(current), current.previous().filter(|_| in_grace)];
        let verified = buckets.into_iter().flatten().any(|bucket| {
            verify_auth_challenge(&key, &self.policy.server, bucket, &signature).is_ok()
        });
        if !verified {
            return Err(QuotaError::BadSignature);
        }
        let expires_at = now
            .checked_add(BEARER_LIFETIME)
            .ok_or(QuotaError::TimeOutOfRange)?;
        self.store.insert_bearer(
            &StoredBearer {
                digest: bearer_digest(bearer),
                account: request.account_id,
                expires_at,
            },
            BEARERS_PER_ACCOUNT,
        )?;
        Ok(expires_at)
    }

    /// The account `bearer` names, if it is valid now.
    ///
    /// # Errors
    ///
    /// Storage errors.
    pub fn account_of(&self, bearer: &BearerToken) -> Result<Option<AccountId>, QuotaError> {
        self.store
            .bearer_account(&bearer_digest(bearer), self.clock.now())
    }

    /// `DELETE /v1/accounts/me`: forgets the key and every bearer of the account. The caller
    /// zeroes its membership leaf ([`crate::Membership::remove`]).
    ///
    /// # Errors
    ///
    /// Storage errors.
    pub fn delete(&self, account: &AccountId) -> Result<(), QuotaError> {
        self.store.delete_account(account)
    }

    /// Forgets expired bearers. Run it periodically; an expired bearer is refused anyway.
    ///
    /// # Errors
    ///
    /// Storage errors.
    pub fn expire(&self) -> Result<(), QuotaError> {
        self.store.prune_bearers(self.clock.now())
    }
}
