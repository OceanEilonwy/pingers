// SPDX-License-Identifier: AGPL-3.0-only
//! The Privacy Pass store contract directly: pruning boundaries the service cannot observe
//! without waiting for memory to grow, plus the day and period arithmetic.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use core::time::Duration;
use std::sync::Arc;

use pingers_anon::{AnonError, IssuerKey};
use pingers_proto::AccountId;
use pingers_quota::{
    Allowance, ISSUANCE_PERIOD, Issuance, IssuanceDay, IssuerKeyRecord, MemoryQuotaStore,
    PrivacyPassStore, QuotaError, REDEMPTION_GRACE, Spend, TokenNonce,
};
use pingers_testkit::seeded_rng;
use pingers_time::UnixSeconds;

#[derive(Debug, thiserror::Error)]
enum TestError {
    #[error(transparent)]
    Quota(#[from] QuotaError),
    #[error(transparent)]
    Anon(#[from] AnonError),
}

const DAY: u64 = 86_400;
const T: u64 = 100 * DAY;

fn at(seconds: u64) -> UnixSeconds {
    UnixSeconds::new(seconds)
}

fn spent_nonces_are_forgotten_only_once_expired(
    store: &impl PrivacyPassStore,
) -> Result<(), TestError> {
    let nonce = TokenNonce::from_bytes([1; 32]);
    assert_eq!(store.spend(nonce, at(T))?, Spend::Fresh);
    store.prune(at(T - 1))?;
    assert_eq!(store.spend(nonce, at(T))?, Spend::AlreadySpent);
    store.prune(at(T))?;
    assert_eq!(store.spend(nonce, at(T))?, Spend::Fresh);
    Ok(())
}

fn keys_are_forgotten_only_once_redemption_ends(
    store: &impl PrivacyPassStore,
) -> Result<(), TestError> {
    store.insert_key(&IssuerKeyRecord {
        key: IssuerKey::generate(&mut seeded_rng(1))?,
        not_before: at(T - 2),
        issue_until: at(T - 1),
        redeem_until: at(T),
    })?;
    store.prune(at(T - 1))?;
    assert_eq!(store.keys()?.len(), 1);
    store.prune(at(T))?;
    assert!(store.keys()?.is_empty());
    Ok(())
}

fn issuance_counts_are_kept_for_today_only(store: &impl PrivacyPassStore) -> Result<(), TestError> {
    let issuance = Issuance {
        account: AccountId::from_bytes([1; 16]),
        day: IssuanceDay::containing(at(T)),
        tokens: 3,
    };
    assert_eq!(store.record_issuance(issuance, 3)?, Allowance::Granted);
    store.prune(at(T + DAY - 1))?;
    assert_eq!(
        store.record_issuance(issuance, 3)?,
        Allowance::Exceeded { issued: 3 }
    );
    store.prune(at(T + DAY))?;
    assert_eq!(store.record_issuance(issuance, 3)?, Allowance::Granted);
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
    spent_nonces_are_forgotten_only_once_expired,
    keys_are_forgotten_only_once_redemption_ends,
    issuance_counts_are_kept_for_today_only,
);

#[test]
fn an_issuance_day_is_a_utc_day() {
    assert_eq!(IssuanceDay::containing(at(0)).get(), 0);
    assert_eq!(IssuanceDay::containing(at(DAY - 1)).get(), 0);
    assert_eq!(IssuanceDay::containing(at(DAY)).get(), 1);
    assert_eq!(IssuanceDay::containing(at(T)).get(), 100);
}

#[test]
fn keys_issue_for_a_week_and_redeem_for_another() {
    assert_eq!(ISSUANCE_PERIOD, Duration::from_secs(7 * DAY));
    assert_eq!(REDEMPTION_GRACE, Duration::from_secs(7 * DAY));
}

#[test]
fn a_token_nonce_does_not_print() {
    let printed = format!("{:?}", TokenNonce::from_bytes([0xAB; 32]));
    assert_eq!(printed, "TokenNonce(redacted)");
}
