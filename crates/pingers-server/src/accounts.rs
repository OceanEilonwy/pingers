// SPDX-License-Identifier: AGPL-3.0-only
//! Accounts and the Acct credential (design B.1, D): registration with a proof of key
//! possession (and an operator's registration code if configured), a bearer from a signed
//! challenge, valid for an hour.
//!
//! In memory. An account is only its public key, and `account_id = SHA-256(pk)[..16]` is
//! deterministic, so after a restart a device registers again and gets the same id; its
//! membership leaf and token counts are pingers-quota's and persist with the quota store.
//! TODO(wave 3 follow-up): pingers-quota has no account table; this belongs there.
//!
//! Wire encodings: `RegisterRequest.proof` is pingers-crypto's `sign_registration` for this
//! server; `AuthRequest.nonce_signature` is its `sign_auth_challenge` for the current bucket
//! (the previous one is accepted for [`CHALLENGE_GRACE`] after a rollover); the bearer is 64 hex
//! digits sent as `Authorization: Bearer <bearer>`.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use pingers_crypto::{
    AccountPublicKey, Signature, account_id, verify_auth_challenge, verify_registration,
};
use pingers_proto::{AccountId, AuthRequest, RegisterRequest, SecretText, ServerId};
use pingers_time::{Bucket, UnixSeconds};
use subtle::ConstantTimeEq as _;

/// How long a bearer is valid (design D: one hour).
pub(crate) const BEARER_LIFETIME: Duration = Duration::from_hours(1);
/// How long after a bucket rollover a challenge signed for the previous bucket still counts.
pub(crate) const CHALLENGE_GRACE: Duration = Duration::from_secs(60);

const BEARER_SCHEME: &str = "Bearer ";

/// Why an account request was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum AccountRefusal {
    /// The proof or challenge signature does not verify, or the account is unknown.
    #[error("signature does not verify")]
    BadSignature,
    /// The server requires a registration code and this one is missing or wrong.
    #[error("registration code missing or wrong")]
    RegistrationCode,
}

struct Bearer {
    account: AccountId,
    expires_at: UnixSeconds,
}

#[derive(Default)]
struct Inner {
    keys: HashMap<AccountId, AccountPublicKey>,
    bearers: HashMap<[u8; 32], Bearer>,
}

/// The accounts this server knows.
pub(crate) struct Accounts {
    server: ServerId,
    codes: Vec<SecretText>,
    inner: Mutex<Inner>,
}

impl Accounts {
    pub(crate) fn new(server: ServerId, codes: Vec<SecretText>) -> Self {
        Self {
            server,
            codes,
            inner: Mutex::new(Inner::default()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // Each method leaves the maps consistent before it can panic.
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn code_accepted(&self, code: Option<&SecretText>) -> bool {
        if self.codes.is_empty() {
            return true;
        }
        let Some(code) = code else {
            return false;
        };
        // Every configured code is compared, so the time taken does not say which matched.
        self.codes
            .iter()
            .fold(subtle::Choice::from(0), |found, known| {
                found | known.ct_eq(code)
            })
            .into()
    }

    /// `POST /v1/accounts`. Registering the same key again is not an error.
    pub(crate) fn register(&self, request: &RegisterRequest) -> Result<AccountId, AccountRefusal> {
        if !self.code_accepted(request.registration_code.as_ref()) {
            return Err(AccountRefusal::RegistrationCode);
        }
        let key = AccountPublicKey::from_bytes(request.signature_key);
        verify_registration(&key, &self.server, &Signature::from_bytes(request.proof))
            .map_err(|_| AccountRefusal::BadSignature)?;
        let id = account_id(&key);
        self.lock().keys.insert(id, key);
        Ok(id)
    }

    /// `POST /v1/auth`: a fresh bearer for `bearer` (random bytes from the caller) if the
    /// challenge for the current bucket, or the previous within the grace, verifies.
    pub(crate) fn authenticate(
        &self,
        request: &AuthRequest,
        now: UnixSeconds,
        bearer: [u8; 32],
    ) -> Result<SecretText, AccountRefusal> {
        let mut inner = self.lock();
        let key = *inner
            .keys
            .get(&request.account_id)
            .ok_or(AccountRefusal::BadSignature)?;
        let signature = Signature::from_bytes(request.nonce_signature);
        let current = now.bucket();
        let in_grace = now.saturating_duration_since(current.start()) < CHALLENGE_GRACE;
        let buckets: [Option<Bucket>; 2] = [Some(current), current.previous().filter(|_| in_grace)];
        let verified = buckets
            .into_iter()
            .flatten()
            .any(|bucket| verify_auth_challenge(&key, &self.server, bucket, &signature).is_ok());
        if !verified {
            return Err(AccountRefusal::BadSignature);
        }
        let expires_at = now
            .checked_add(BEARER_LIFETIME)
            .ok_or(AccountRefusal::BadSignature)?;
        inner.bearers.insert(
            bearer,
            Bearer {
                account: request.account_id,
                expires_at,
            },
        );
        Ok(SecretText::new(hex::encode(bearer)))
    }

    /// The account an `Authorization: Bearer` value names, if it is valid at `now`.
    pub(crate) fn account_of(&self, authorization: &str, now: UnixSeconds) -> Option<AccountId> {
        let bearer = crate::wire::hex_array::<32>(authorization.strip_prefix(BEARER_SCHEME)?)?;
        let inner = self.lock();
        let found = inner.bearers.get(&bearer)?;
        (now < found.expires_at && inner.keys.contains_key(&found.account)).then_some(found.account)
    }

    /// `DELETE /v1/accounts/me`: forgets the key and every bearer of the account.
    pub(crate) fn delete(&self, account: &AccountId) {
        let mut inner = self.lock();
        inner.keys.remove(account);
        inner.bearers.retain(|_, bearer| bearer.account != *account);
    }

    /// Drops expired bearers (the sweep).
    pub(crate) fn expire(&self, now: UnixSeconds) {
        self.lock()
            .bearers
            .retain(|_, bearer| now < bearer.expires_at);
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_crypto::{AccountSigningKey, sign_auth_challenge, sign_registration};
    use zeroize::Zeroizing;

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const START: UnixSeconds = UnixSeconds::new(1_800_000_000);

    fn server() -> Result<ServerId, pingers_proto::ProtoError> {
        ServerId::new("https://pingers.example".to_owned())
    }

    fn key(fill: u8) -> AccountSigningKey {
        AccountSigningKey::from_bytes(Zeroizing::new([fill; 32]))
    }

    fn registration(
        key: &AccountSigningKey,
        code: Option<&str>,
    ) -> Result<RegisterRequest, Box<dyn std::error::Error>> {
        Ok(RegisterRequest {
            signature_key: *key.public_key().as_bytes(),
            proof: *sign_registration(key, &server()?)?.as_bytes(),
            registration_code: code.map(|code| SecretText::new(code.to_owned())),
        })
    }

    fn auth(
        key: &AccountSigningKey,
        bucket: Bucket,
    ) -> Result<AuthRequest, Box<dyn std::error::Error>> {
        Ok(AuthRequest {
            account_id: account_id(&key.public_key()),
            nonce_signature: *sign_auth_challenge(key, &server()?, bucket)?.as_bytes(),
        })
    }

    #[test]
    fn registration_codes_gate_signups_when_configured() -> TestResult {
        let open = Accounts::new(server()?, Vec::new());
        assert!(open.register(&registration(&key(1), None)?).is_ok());
        let gated = Accounts::new(
            server()?,
            vec![
                SecretText::new("alpha".to_owned()),
                SecretText::new("beta".to_owned()),
            ],
        );
        assert_eq!(
            gated.register(&registration(&key(1), None)?),
            Err(AccountRefusal::RegistrationCode)
        );
        assert_eq!(
            gated.register(&registration(&key(1), Some("gamma"))?),
            Err(AccountRefusal::RegistrationCode)
        );
        assert_eq!(
            gated.register(&registration(&key(1), Some("beta"))?),
            Ok(account_id(&key(1).public_key()))
        );
        // A code listed twice still matches.
        let doubled = Accounts::new(
            server()?,
            vec![
                SecretText::new("alpha".to_owned()),
                SecretText::new("alpha".to_owned()),
            ],
        );
        assert!(
            doubled
                .register(&registration(&key(3), Some("alpha"))?)
                .is_ok()
        );
        assert!(
            gated
                .register(&registration(&key(2), Some("alpha"))?)
                .is_ok()
        );
        Ok(())
    }

    #[test]
    fn a_proof_for_another_key_is_refused() -> TestResult {
        let accounts = Accounts::new(server()?, Vec::new());
        let mut forged = registration(&key(1), None)?;
        forged.signature_key = *key(2).public_key().as_bytes();
        assert_eq!(
            accounts.register(&forged),
            Err(AccountRefusal::BadSignature)
        );
        Ok(())
    }

    #[test]
    fn bearers_need_a_current_challenge_and_last_an_hour() -> TestResult {
        let accounts = Accounts::new(server()?, Vec::new());
        let id = accounts.register(&registration(&key(1), None)?)?;
        let bucket = START.bucket();
        // An unknown account and a stale challenge get nothing.
        assert!(
            accounts
                .authenticate(&auth(&key(2), bucket)?, START, [1; 32])
                .is_err()
        );
        let old = bucket.previous().ok_or("bucket")?;
        let late = START.checked_add(Duration::from_mins(5)).ok_or("time")?;
        assert!(
            accounts
                .authenticate(&auth(&key(1), old)?, late, [1; 32])
                .is_err()
        );
        let bearer = accounts.authenticate(&auth(&key(1), bucket)?, START, [3; 32])?;
        let header = format!("Bearer {}", bearer.expose());
        assert_eq!(accounts.account_of(&header, START), Some(id));
        assert_eq!(
            accounts.account_of(&format!("Bearer {}", hex::encode([4; 32])), START),
            None
        );
        assert_eq!(accounts.account_of(bearer.expose(), START), None);
        let almost = START
            .checked_add(BEARER_LIFETIME.saturating_sub(Duration::from_secs(1)))
            .ok_or("time")?;
        assert_eq!(accounts.account_of(&header, almost), Some(id));
        let expired = START.checked_add(BEARER_LIFETIME).ok_or("time")?;
        assert_eq!(accounts.account_of(&header, expired), None);
        accounts.expire(almost);
        assert_eq!(accounts.lock().bearers.len(), 1);
        accounts.expire(expired);
        assert!(accounts.lock().bearers.is_empty());
        Ok(())
    }

    #[test]
    fn the_previous_bucket_counts_only_just_after_a_rollover() -> TestResult {
        let accounts = Accounts::new(server()?, Vec::new());
        accounts.register(&registration(&key(1), None)?)?;
        let next = START.bucket().next().ok_or("bucket")?;
        let rollover = next.start();
        let within = rollover
            .checked_add(CHALLENGE_GRACE.saturating_sub(Duration::from_secs(1)))
            .ok_or("time")?;
        let after = rollover.checked_add(CHALLENGE_GRACE).ok_or("time")?;
        let signed_before = auth(&key(1), START.bucket())?;
        assert!(
            accounts
                .authenticate(&signed_before, within, [1; 32])
                .is_ok()
        );
        assert!(
            accounts
                .authenticate(&signed_before, after, [2; 32])
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn deleting_an_account_ends_its_bearers() -> TestResult {
        let accounts = Accounts::new(server()?, Vec::new());
        let id = accounts.register(&registration(&key(1), None)?)?;
        accounts.register(&registration(&key(2), None)?)?;
        let mine = accounts.authenticate(&auth(&key(1), START.bucket())?, START, [1; 32])?;
        let theirs = accounts.authenticate(&auth(&key(2), START.bucket())?, START, [2; 32])?;
        accounts.delete(&id);
        assert_eq!(
            accounts.account_of(&format!("Bearer {}", mine.expose()), START),
            None
        );
        assert!(
            accounts
                .account_of(&format!("Bearer {}", theirs.expose()), START)
                .is_some()
        );
        assert!(
            accounts
                .authenticate(&auth(&key(1), START.bucket())?, START, [5; 32])
                .is_err()
        );
        Ok(())
    }
}
