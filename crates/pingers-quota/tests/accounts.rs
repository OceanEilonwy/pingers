// SPDX-License-Identifier: AGPL-3.0-only
//! Accounts and bearers: the store contract against every store, the service's rules, and
//! what survives reopening the SQLite file.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use core::num::NonZeroU32;
use core::time::Duration;
use std::sync::Arc;

use pingers_crypto::{
    AccountSigningKey, BearerDigest, BearerToken, CryptoError, account_id, sign_auth_challenge,
    sign_registration,
};
use pingers_proto::{AccountId, AuthRequest, ProtoError, RegisterRequest, SecretText, ServerId};
use pingers_quota::{
    AccountInsert, AccountPolicy, AccountStore, Accounts, BEARER_LIFETIME, BEARERS_PER_ACCOUNT,
    CHALLENGE_GRACE, MemoryQuotaStore, NewAccount, QuotaError, SIGNUP_WINDOW, SignupWindow,
    SignupsPerHour, StoredBearer,
};
use pingers_testkit::{ManualClock, test_clock};
use pingers_time::{Bucket, Clock as _, UnixSeconds};
use zeroize::Zeroizing;

#[derive(Debug, thiserror::Error)]
enum TestError {
    #[error(transparent)]
    Quota(#[from] QuotaError),
    #[error(transparent)]
    Crypto(#[from] CryptoError),
    #[error(transparent)]
    Proto(#[from] ProtoError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Missing(&'static str),
}

const T: u64 = 1_800_000_000;

fn at(seconds: u64) -> UnixSeconds {
    UnixSeconds::new(seconds)
}

fn new_account(tag: u8, created_at: u64) -> NewAccount {
    NewAccount {
        id: AccountId::from_bytes([tag; 16]),
        key: key(tag).public_key(),
        created_at: at(created_at),
    }
}

fn bearer(tag: u8, account: u8, expires_at: u64) -> StoredBearer {
    StoredBearer {
        digest: BearerDigest::from_bytes([tag; 32]),
        account: AccountId::from_bytes([account; 16]),
        expires_at: at(expires_at),
    }
}

fn window(since: u64, max: u32) -> SignupWindow {
    SignupWindow {
        since: at(since),
        max,
    }
}

// --- the store contract ---

fn an_account_is_stored_once_and_counts_toward_its_window(
    store: &impl AccountStore,
) -> Result<(), TestError> {
    let first = new_account(1, T);
    assert_eq!(
        store.insert_account(&first, window(T, 1))?,
        AccountInsert::Created
    );
    assert_eq!(
        store.insert_account(&first, window(T, 1))?,
        AccountInsert::Existing
    );
    assert_eq!(store.account_key(&first.id)?, Some(first.key));
    let second = new_account(2, T);
    assert_eq!(
        store.insert_account(&second, window(T, 1))?,
        AccountInsert::WindowFull
    );
    assert_eq!(store.account_key(&second.id)?, None);
    // An account made before the window does not count.
    assert_eq!(
        store.insert_account(&second, window(T + 1, 1))?,
        AccountInsert::Created
    );
    Ok(())
}

fn a_bearer_names_its_account_until_it_expires_or_the_account_goes(
    store: &impl AccountStore,
) -> Result<(), TestError> {
    let account = new_account(1, T);
    let _ = store.insert_account(&account, window(T, 10))?;
    let stored = bearer(9, 1, T + 10);
    store.insert_bearer(&stored, 4)?;
    assert_eq!(
        store.bearer_account(&stored.digest, at(T + 9))?,
        Some(account.id)
    );
    assert_eq!(store.bearer_account(&stored.digest, at(T + 10))?, None);
    assert_eq!(
        store.bearer_account(&BearerDigest::from_bytes([8; 32]), at(T))?,
        None
    );
    store.delete_account(&account.id)?;
    assert_eq!(store.bearer_account(&stored.digest, at(T))?, None);
    assert_eq!(store.account_key(&account.id)?, None);
    // Coming back does not revive the old bearer.
    let _ = store.insert_account(&account, window(T, 10))?;
    assert_eq!(store.bearer_account(&stored.digest, at(T))?, None);
    Ok(())
}

fn an_account_keeps_its_newest_bearers_only(store: &impl AccountStore) -> Result<(), TestError> {
    let _ = store.insert_account(&new_account(1, T), window(T, 10))?;
    let _ = store.insert_account(&new_account(2, T), window(T, 10))?;
    let theirs = bearer(20, 2, T + 1);
    store.insert_bearer(&theirs, 2)?;
    let oldest = bearer(10, 1, T + 1);
    let middle = bearer(11, 1, T + 2);
    let newest = bearer(12, 1, T + 3);
    for stored in [&oldest, &middle, &newest] {
        store.insert_bearer(stored, 2)?;
    }
    assert_eq!(store.bearer_account(&oldest.digest, at(T))?, None);
    assert!(store.bearer_account(&middle.digest, at(T))?.is_some());
    assert!(store.bearer_account(&newest.digest, at(T))?.is_some());
    assert!(store.bearer_account(&theirs.digest, at(T))?.is_some());
    // The bearer just made stays even when it expires before the ones already held.
    let early = bearer(13, 1, T + 1);
    store.insert_bearer(&early, 2)?;
    assert!(store.bearer_account(&early.digest, at(T))?.is_some());
    assert!(store.bearer_account(&newest.digest, at(T))?.is_some());
    assert_eq!(store.bearer_account(&middle.digest, at(T))?, None);
    Ok(())
}

fn pruning_drops_expired_bearers_only(store: &impl AccountStore) -> Result<(), TestError> {
    let _ = store.insert_account(&new_account(1, T), window(T, 10))?;
    let ending = bearer(1, 1, T + 1);
    let lasting = bearer(2, 1, T + 2);
    store.insert_bearer(&ending, 4)?;
    store.insert_bearer(&lasting, 4)?;
    store.prune_bearers(at(T + 1))?;
    // Expired bearers are refused anyway; after pruning, keeping 2 must not evict `lasting`.
    store.insert_bearer(&bearer(3, 1, T + 3), 2)?;
    assert!(store.bearer_account(&lasting.digest, at(T))?.is_some());
    Ok(())
}

macro_rules! for_each_store {
    ($($test:ident),* $(,)?) => {
        mod memory {
            use super::*;
            $(
                #[test]
                fn $test() -> Result<(), TestError> {
                    super::$test(&MemoryQuotaStore::new())
                }
            )*
        }
        mod shared {
            use super::*;
            $(
                #[test]
                fn $test() -> Result<(), TestError> {
                    super::$test(&Arc::new(MemoryQuotaStore::new()))
                }
            )*
        }
        #[cfg(feature = "sqlite")]
        mod sqlite {
            use super::*;
            $(
                #[test]
                fn $test() -> Result<(), TestError> {
                    super::$test(&pingers_quota::SqliteQuotaStore::open_in_memory()?)
                }
            )*
        }
    };
}

for_each_store!(
    an_account_is_stored_once_and_counts_toward_its_window,
    a_bearer_names_its_account_until_it_expires_or_the_account_goes,
    an_account_keeps_its_newest_bearers_only,
    pruning_drops_expired_bearers_only,
);

// --- the service ---

fn server() -> Result<ServerId, TestError> {
    Ok(ServerId::new("https://pingers.example".to_owned())?)
}

fn key(fill: u8) -> AccountSigningKey {
    AccountSigningKey::from_bytes(Zeroizing::new([fill; 32]))
}

fn token(fill: u8) -> BearerToken {
    BearerToken::from_bytes(Zeroizing::new([fill; 32]))
}

fn code(text: &str) -> SecretText {
    SecretText::new(text.to_owned())
}

fn registration(key: &AccountSigningKey, code: Option<&str>) -> Result<RegisterRequest, TestError> {
    Ok(RegisterRequest {
        signature_key: *key.public_key().as_bytes(),
        proof: *sign_registration(key, &server()?)?.as_bytes(),
        registration_code: code.map(|text| SecretText::new(text.to_owned())),
    })
}

fn auth(key: &AccountSigningKey, bucket: Bucket) -> Result<AuthRequest, TestError> {
    Ok(AuthRequest {
        account_id: account_id(&key.public_key()),
        nonce_signature: *sign_auth_challenge(key, &server()?, bucket)?.as_bytes(),
    })
}

fn signups(limit: u32) -> Result<SignupsPerHour, TestError> {
    Ok(SignupsPerHour::new(
        NonZeroU32::new(limit).ok_or(TestError::Missing("limit"))?,
    ))
}

fn policy(codes: &[&str]) -> Result<AccountPolicy, TestError> {
    Ok(AccountPolicy {
        server: server()?,
        registration_codes: codes.iter().map(|text| code(text)).collect(),
        signups_per_hour: signups(100)?,
    })
}

fn service<S: AccountStore>(
    store: S,
    codes: &[&str],
) -> Result<(ManualClock, Accounts<S>), TestError> {
    let clock = test_clock();
    let accounts = Accounts::new(store, Arc::new(clock.clone()), policy(codes)?);
    Ok((clock, accounts))
}

#[test]
fn registration_codes_gate_new_accounts_when_configured() -> Result<(), TestError> {
    let (_clock, open) = service(MemoryQuotaStore::new(), &[])?;
    assert!(open.register(&registration(&key(1), None)?).is_ok());
    let (_clock, gated) = service(MemoryQuotaStore::new(), &["alpha", "beta"])?;
    assert!(matches!(
        gated.register(&registration(&key(1), None)?),
        Err(QuotaError::RegistrationCode)
    ));
    assert!(matches!(
        gated.register(&registration(&key(1), Some("gamma"))?),
        Err(QuotaError::RegistrationCode)
    ));
    assert_eq!(
        gated.register(&registration(&key(1), Some("beta"))?)?,
        account_id(&key(1).public_key())
    );
    assert!(
        gated
            .register(&registration(&key(2), Some("alpha"))?)
            .is_ok()
    );
    // A code listed twice still matches.
    let (_clock, doubled) = service(MemoryQuotaStore::new(), &["alpha", "alpha"])?;
    assert!(
        doubled
            .register(&registration(&key(3), Some("alpha"))?)
            .is_ok()
    );
    Ok(())
}

#[test]
fn a_known_key_registers_again_without_a_code() -> Result<(), TestError> {
    // The operator changed the code after this device registered: its proof is enough.
    let store = Arc::new(MemoryQuotaStore::new());
    let (_clock, before) = service(Arc::clone(&store), &["old"])?;
    let id = before.register(&registration(&key(1), Some("old"))?)?;
    let (_clock, after) = service(store, &["new"])?;
    assert_eq!(after.register(&registration(&key(1), Some("old"))?)?, id);
    assert_eq!(after.register(&registration(&key(1), None)?)?, id);
    assert!(matches!(
        after.register(&registration(&key(2), Some("old"))?),
        Err(QuotaError::RegistrationCode)
    ));
    Ok(())
}

#[test]
fn a_proof_for_another_key_or_server_is_refused() -> Result<(), TestError> {
    let (_clock, accounts) = service(MemoryQuotaStore::new(), &[])?;
    let mut forged = registration(&key(1), None)?;
    forged.signature_key = *key(2).public_key().as_bytes();
    assert!(matches!(
        accounts.register(&forged),
        Err(QuotaError::BadSignature)
    ));
    let elsewhere = RegisterRequest {
        signature_key: *key(1).public_key().as_bytes(),
        proof: *sign_registration(&key(1), &ServerId::new("https://other.example".to_owned())?)?
            .as_bytes(),
        registration_code: None,
    };
    assert!(matches!(
        accounts.register(&elsewhere),
        Err(QuotaError::BadSignature)
    ));
    Ok(())
}

#[test]
fn new_accounts_are_limited_per_hour() -> Result<(), TestError> {
    let clock = test_clock();
    let accounts = Accounts::new(
        MemoryQuotaStore::new(),
        Arc::new(clock.clone()),
        AccountPolicy {
            server: server()?,
            registration_codes: Vec::new(),
            signups_per_hour: signups(2)?,
        },
    );
    accounts.register(&registration(&key(1), None)?)?;
    clock.advance(Duration::from_mins(30));
    accounts.register(&registration(&key(2), None)?)?;
    assert!(matches!(
        accounts.register(&registration(&key(3), None)?),
        Err(QuotaError::SignupLimit)
    ));
    // Known keys are not new accounts.
    assert!(accounts.register(&registration(&key(1), None)?).is_ok());
    // An hour after the first, there is room for one more.
    clock.advance(SIGNUP_WINDOW.saturating_sub(Duration::from_mins(30)));
    assert!(matches!(
        accounts.register(&registration(&key(3), None)?),
        Err(QuotaError::SignupLimit)
    ));
    clock.advance(Duration::from_secs(1));
    assert!(accounts.register(&registration(&key(3), None)?).is_ok());
    assert!(matches!(
        accounts.register(&registration(&key(4), None)?),
        Err(QuotaError::SignupLimit)
    ));
    Ok(())
}

#[test]
fn bearers_need_a_current_challenge_and_last_an_hour() -> Result<(), TestError> {
    let (clock, accounts) = service(MemoryQuotaStore::new(), &[])?;
    let id = accounts.register(&registration(&key(1), None)?)?;
    let start = clock.now();
    let bucket = start.bucket();
    // An unknown account and a stale challenge get nothing.
    assert!(matches!(
        accounts.authenticate(&auth(&key(2), bucket)?, &token(1)),
        Err(QuotaError::BadSignature)
    ));
    let old = bucket.previous().ok_or(TestError::Missing("bucket"))?;
    clock.advance(CHALLENGE_GRACE);
    assert!(matches!(
        accounts.authenticate(&auth(&key(1), old)?, &token(1)),
        Err(QuotaError::BadSignature)
    ));
    let expires_at = accounts.authenticate(&auth(&key(1), clock.now().bucket())?, &token(3))?;
    assert_eq!(Some(expires_at), clock.now().checked_add(BEARER_LIFETIME));
    assert_eq!(accounts.account_of(&token(3))?, Some(id));
    assert_eq!(accounts.account_of(&token(4))?, None);
    clock.advance(BEARER_LIFETIME.saturating_sub(Duration::from_secs(1)));
    assert_eq!(accounts.account_of(&token(3))?, Some(id));
    clock.advance(Duration::from_secs(1));
    assert_eq!(accounts.account_of(&token(3))?, None);
    accounts.expire()?;
    assert_eq!(accounts.account_of(&token(3))?, None);
    Ok(())
}

#[test]
fn the_previous_bucket_counts_only_just_after_a_rollover() -> Result<(), TestError> {
    let (clock, accounts) = service(MemoryQuotaStore::new(), &[])?;
    accounts.register(&registration(&key(1), None)?)?;
    let signed_before = auth(&key(1), clock.now().bucket())?;
    let next = clock
        .now()
        .bucket()
        .next()
        .ok_or(TestError::Missing("bucket"))?;
    clock.advance(
        next.start()
            .saturating_duration_since(clock.now())
            .saturating_add(CHALLENGE_GRACE)
            .saturating_sub(Duration::from_secs(1)),
    );
    assert!(accounts.authenticate(&signed_before, &token(1)).is_ok());
    clock.advance(Duration::from_secs(1));
    assert!(matches!(
        accounts.authenticate(&signed_before, &token(2)),
        Err(QuotaError::BadSignature)
    ));
    Ok(())
}

#[test]
fn an_account_holds_a_bounded_number_of_bearers() -> Result<(), TestError> {
    let (clock, accounts) = service(MemoryQuotaStore::new(), &[])?;
    accounts.register(&registration(&key(1), None)?)?;
    let challenge = auth(&key(1), clock.now().bucket())?;
    let count = u8::try_from(BEARERS_PER_ACCOUNT).map_err(|_| TestError::Missing("count"))?;
    for fill in 0..=count {
        let _expires_at = accounts.authenticate(&challenge, &token(fill))?;
        clock.advance(Duration::from_secs(1));
    }
    assert_eq!(accounts.account_of(&token(0))?, None);
    for fill in 1..=count {
        assert!(accounts.account_of(&token(fill))?.is_some());
    }
    Ok(())
}

#[test]
fn deleting_an_account_ends_its_bearers_only() -> Result<(), TestError> {
    let (clock, accounts) = service(MemoryQuotaStore::new(), &[])?;
    let mine = accounts.register(&registration(&key(1), None)?)?;
    accounts.register(&registration(&key(2), None)?)?;
    let bucket = clock.now().bucket();
    let _ = accounts.authenticate(&auth(&key(1), bucket)?, &token(1))?;
    let _ = accounts.authenticate(&auth(&key(2), bucket)?, &token(2))?;
    accounts.delete(&mine)?;
    assert_eq!(accounts.account_of(&token(1))?, None);
    assert!(accounts.account_of(&token(2))?.is_some());
    assert!(matches!(
        accounts.authenticate(&auth(&key(1), bucket)?, &token(5)),
        Err(QuotaError::BadSignature)
    ));
    Ok(())
}

// --- persistence ---

#[cfg(feature = "sqlite")]
fn scratch(name: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "pingers-quota-{name}-{}.sqlite",
        std::process::id()
    ));
    for suffix in ["", "-wal", "-shm"] {
        let _stale = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
    path
}

#[cfg(feature = "sqlite")]
#[test]
fn accounts_and_bearers_survive_reopening() -> Result<(), TestError> {
    use pingers_quota::SqliteQuotaStore;

    let path = scratch("accounts-reopen");
    let clock = test_clock();
    let id = {
        let accounts = Accounts::new(
            SqliteQuotaStore::open(&path)?,
            Arc::new(clock.clone()),
            policy(&["old"])?,
        );
        let id = accounts.register(&registration(&key(1), Some("old"))?)?;
        let _ = accounts.authenticate(&auth(&key(1), clock.now().bucket())?, &token(7))?;
        accounts.register(&registration(&key(2), Some("old"))?)?;
        accounts.delete(&account_id(&key(2).public_key()))?;
        id
    };
    // A restart with a new code: the bearer still works, the device needs no code, the
    // deleted account stays deleted, and the bearer still ends an hour after it was made.
    clock.advance(Duration::from_mins(10));
    let accounts = Accounts::new(
        SqliteQuotaStore::open(&path)?,
        Arc::new(clock.clone()),
        policy(&["new"])?,
    );
    assert_eq!(accounts.account_of(&token(7))?, Some(id));
    assert_eq!(accounts.register(&registration(&key(1), None)?)?, id);
    assert!(matches!(
        accounts.authenticate(&auth(&key(2), clock.now().bucket())?, &token(8)),
        Err(QuotaError::BadSignature)
    ));
    clock.advance(Duration::from_mins(50));
    assert_eq!(accounts.account_of(&token(7))?, None);
    Ok(())
}

#[cfg(feature = "sqlite")]
#[test]
fn a_deleted_account_leaves_no_bytes_in_the_files() -> Result<(), TestError> {
    use pingers_quota::SqliteQuotaStore;

    let path = scratch("accounts-secure-delete");
    let (clock, accounts) = service(SqliteQuotaStore::open(&path)?, &[])?;
    let id = accounts.register(&registration(&key(1), None)?)?;
    let _ = accounts.authenticate(&auth(&key(1), clock.now().bucket())?, &token(7))?;
    accounts.delete(&id)?;
    let public_key = *key(1).public_key().as_bytes();
    for file in [path.clone(), path.with_extension("sqlite-wal")] {
        let bytes = std::fs::read(&file).unwrap_or_default();
        assert!(
            !bytes
                .windows(public_key.len())
                .any(|window| window == public_key.as_slice()),
            "the deleted account's key is still in {}",
            file.display()
        );
    }
    Ok(())
}
