// SPDX-License-Identifier: AGPL-3.0-only
//! The Privacy Pass issuer: issuance, the daily allowance, key rotation and the spent set.
//! Run against every store implementation.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use core::time::Duration;
use std::sync::Arc;

use pingers_anon::{
    AnonError, IssuerKey, PrivacyPassToken, TokenCount, begin_token_issuance, issue_tokens,
};
use pingers_proto::{AccountId, TokenAllowance};
use pingers_quota::{
    DAILY_TOKEN_LIMIT, ISSUANCE_PERIOD, IssuanceDay, MemoryQuotaStore, PrivacyPass,
    PrivacyPassStore, QuotaError, REDEMPTION_GRACE,
};
use pingers_testkit::{ManualClock, seeded_rng, test_clock};

#[derive(Debug, thiserror::Error)]
enum TestError {
    #[error(transparent)]
    Quota(#[from] QuotaError),
    #[error(transparent)]
    Anon(#[from] AnonError),
    #[error("fixture: {0}")]
    Fixture(&'static str),
}

const ALICE: AccountId = AccountId::from_bytes([1; 16]);
const BOB: AccountId = AccountId::from_bytes([2; 16]);

struct World<S> {
    clock: ManualClock,
    issuer: PrivacyPass<S>,
}

impl<S: PrivacyPassStore> World<S> {
    fn new(store: S) -> Result<Self, TestError> {
        let clock = test_clock();
        let issuer = PrivacyPass::new(store, Arc::new(clock.clone()))?;
        Ok(Self { clock, issuer })
    }

    /// The newest published key, as a client would pick it.
    fn current_key(&self) -> Result<Vec<u8>, TestError> {
        // A fresh seed per instant, so a rotation really makes a new key.
        self.issuer
            .rotate(&mut seeded_rng(pingers_time::Clock::now(&self.clock).get()))?;
        self.issuer
            .public_keys()?
            .pop()
            .map(|key| key.public_key)
            .ok_or(TestError::Fixture("no key"))
    }

    /// Fetches `count` tokens for `account` against `public_key`.
    fn fetch(
        &self,
        account: AccountId,
        public_key: &[u8],
        count: u16,
    ) -> Result<Vec<PrivacyPassToken>, TestError> {
        let mut rng = seeded_rng(u64::from(count));
        let pending = begin_token_issuance(public_key, TokenCount::new(count)?, &mut rng)?;
        let issued = self.issuer.issue(account, pending.request(), &mut rng)?;
        Ok(pending.finish(&issued.response)?)
    }
}

fn issued_tokens_redeem_once_each(store: impl PrivacyPassStore) -> Result<(), TestError> {
    let world = World::new(store)?;
    let key = world.current_key()?;
    let tokens = world.fetch(ALICE, &key, 3)?;
    assert_eq!(tokens.len(), 3);
    for token in &tokens {
        world.issuer.redeem(token.as_bytes())?;
    }
    for token in &tokens {
        assert!(matches!(
            world.issuer.redeem(token.as_bytes()),
            Err(QuotaError::DoubleSpend)
        ));
    }
    Ok(())
}

fn the_daily_allowance_is_per_account_and_per_day(
    store: impl PrivacyPassStore,
) -> Result<(), TestError> {
    let world = World::new(store)?;
    let key = world.current_key()?;
    let most = DAILY_TOKEN_LIMIT.saturating_sub(2);
    assert_eq!(world.fetch(ALICE, &key, most)?.len(), usize::from(most));
    assert_eq!(
        world.fetch(ALICE, &key, 2)?.len(),
        2,
        "exactly the limit is allowed"
    );
    assert!(matches!(
        world.fetch(ALICE, &key, 1),
        Err(TestError::Quota(QuotaError::DailyLimit(allowance)))
            if allowance.remaining == TokenAllowance::NONE
    ));
    assert_eq!(world.fetch(BOB, &key, 1)?.len(), 1);
    world.clock.advance(Duration::from_hours(24));
    let key = world.current_key()?;
    assert_eq!(world.fetch(ALICE, &key, 1)?.len(), 1);
    Ok(())
}

fn a_batch_over_the_allowance_is_refused_whole(
    store: impl PrivacyPassStore,
) -> Result<(), TestError> {
    let world = World::new(store)?;
    let key = world.current_key()?;
    let _first = world.fetch(ALICE, &key, 2)?;
    let over = DAILY_TOKEN_LIMIT.saturating_sub(1);
    let refused = world.fetch(ALICE, &key, over);
    let Err(TestError::Quota(QuotaError::DailyLimit(allowance))) = refused else {
        return Err(TestError::Fixture("not refused for the daily limit"));
    };
    assert_eq!(
        allowance.remaining,
        TokenAllowance::new(DAILY_TOKEN_LIMIT.saturating_sub(2)),
        "the refusal says what is left"
    );
    let today = IssuanceDay::containing(pingers_time::Clock::now(&world.clock));
    assert_eq!(Some(allowance.resets_at), today.next());
    let rest = DAILY_TOKEN_LIMIT.saturating_sub(2);
    assert_eq!(
        world.fetch(ALICE, &key, rest)?.len(),
        usize::from(rest),
        "nothing was counted"
    );
    Ok(())
}

fn a_token_from_another_issuer_is_refused(store: impl PrivacyPassStore) -> Result<(), TestError> {
    let world = World::new(store)?;
    let _ours = world.current_key()?;
    let stranger = IssuerKey::generate(&mut seeded_rng(5))?;
    let mut rng = seeded_rng(6);
    let pending = begin_token_issuance(&stranger.public_key(), TokenCount::new(1)?, &mut rng)?;
    let response = issue_tokens(&stranger, pending.request(), &mut rng)?;
    let tokens = pending.finish(&response)?;
    let token = tokens.first().ok_or(TestError::Fixture("no token"))?;
    assert!(matches!(
        world.issuer.redeem(token.as_bytes()),
        Err(QuotaError::Anon(AnonError::BadToken))
    ));
    assert!(matches!(
        world.issuer.redeem(b"not a token"),
        Err(QuotaError::Anon(AnonError::BadToken))
    ));
    Ok(())
}

fn keys_rotate_weekly_and_old_tokens_redeem_for_a_grace_week(
    store: impl PrivacyPassStore,
) -> Result<(), TestError> {
    let world = World::new(store)?;
    let first_key = world.current_key()?;
    let old_tokens = world.fetch(ALICE, &first_key, 2)?;
    world
        .clock
        .advance(ISSUANCE_PERIOD.saturating_sub(Duration::from_secs(1)));
    assert_eq!(world.current_key()?, first_key, "still issuing");
    world.clock.advance(Duration::from_secs(1));
    let second_key = world.current_key()?;
    assert_ne!(second_key, first_key);
    assert_eq!(world.issuer.public_keys()?.len(), 2);
    assert!(matches!(
        world.fetch(ALICE, &first_key, 1),
        Err(TestError::Anon(AnonError::BadIssuance)
            | TestError::Quota(QuotaError::Anon(AnonError::BadIssuance)))
    ));
    let first = old_tokens.first().ok_or(TestError::Fixture("no token"))?;
    world.issuer.redeem(first.as_bytes())?;
    world.clock.advance(REDEMPTION_GRACE);
    let _third = world.current_key()?;
    let second = old_tokens.get(1).ok_or(TestError::Fixture("no token"))?;
    assert!(matches!(
        world.issuer.redeem(second.as_bytes()),
        Err(QuotaError::Anon(AnonError::BadToken))
    ));
    let published = world.issuer.public_keys()?;
    assert!(published.iter().all(|key| key.public_key != first_key));
    Ok(())
}

fn a_key_stops_being_published_when_redemption_ends(
    store: impl PrivacyPassStore,
) -> Result<(), TestError> {
    let world = World::new(store)?;
    let key = world.current_key()?;
    let tokens = world.fetch(ALICE, &key, 1)?;
    world.clock.advance(
        ISSUANCE_PERIOD
            .saturating_add(REDEMPTION_GRACE)
            .saturating_sub(Duration::from_secs(1)),
    );
    assert!(
        world
            .issuer
            .public_keys()?
            .iter()
            .any(|published| published.public_key == key)
    );
    world.clock.advance(Duration::from_secs(1));
    // No rotation has run: expiry alone stops the key.
    assert!(world.issuer.public_keys()?.is_empty());
    let token = tokens.first().ok_or(TestError::Fixture("no token"))?;
    assert!(matches!(
        world.issuer.redeem(token.as_bytes()),
        Err(QuotaError::Anon(AnonError::BadToken))
    ));
    Ok(())
}

fn keys_and_spent_tokens_survive_a_restart<S: PrivacyPassStore>(store: S) -> Result<(), TestError> {
    let store = Arc::new(store);
    let clock = test_clock();
    let first = PrivacyPass::new(Arc::clone(&store), Arc::new(clock.clone()))?;
    first.rotate(&mut seeded_rng(1))?;
    let key = first.public_keys()?;
    let world = World {
        clock: clock.clone(),
        issuer: first,
    };
    let public_key = key
        .first()
        .map(|key| key.public_key.clone())
        .ok_or(TestError::Fixture("no key"))?;
    let tokens = world.fetch(ALICE, &public_key, 2)?;
    let spent = tokens.first().ok_or(TestError::Fixture("no token"))?;
    world.issuer.redeem(spent.as_bytes())?;
    drop(world);

    let second = PrivacyPass::new(Arc::clone(&store), Arc::new(clock))?;
    assert_eq!(second.public_keys()?, key);
    assert!(matches!(
        second.redeem(spent.as_bytes()),
        Err(QuotaError::DoubleSpend)
    ));
    let unspent = tokens.get(1).ok_or(TestError::Fixture("no token"))?;
    second.redeem(unspent.as_bytes())?;
    Ok(())
}

macro_rules! for_each_store {
    ($($test:ident),* $(,)?) => {
        mod memory {
            use super::*;
            $(
                #[test]
                fn $test() -> Result<(), TestError> {
                    super::$test(MemoryQuotaStore::new())
                }
            )*
        }
        #[cfg(feature = "sqlite")]
        mod sqlite {
            use super::*;
            $(
                #[test]
                fn $test() -> Result<(), TestError> {
                    super::$test(pingers_quota::SqliteQuotaStore::open_in_memory()?)
                }
            )*
        }
    };
}

for_each_store!(
    issued_tokens_redeem_once_each,
    the_daily_allowance_is_per_account_and_per_day,
    a_batch_over_the_allowance_is_refused_whole,
    a_token_from_another_issuer_is_refused,
    keys_rotate_weekly_and_old_tokens_redeem_for_a_grace_week,
    a_key_stops_being_published_when_redemption_ends,
    keys_and_spent_tokens_survive_a_restart,
);
