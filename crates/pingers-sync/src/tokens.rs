// SPDX-License-Identifier: AGPL-3.0-only
//! The Privacy Pass stock (design B.4.8): the main app fetches tokens in batches over the
//! authenticated API and keeps a minimum in the shared store for extensions and fallback.
//!
//! The server issues each account at most `Params.privacy_pass_daily_limit` tokens per UTC day
//! and refuses a batch that would pass it whole (429). So a refill is sized from the allowance:
//! the device records the advertised limit and what it was issued today (`pp_allowance`), and
//! asks for the shortfall or what is left of today's allowance, whichever is smaller. A day's
//! allowance spent leaves the stock as it is until the next UTC day; tokens stay redeemable for
//! up to two weeks, so a stock topped up on one day carries over.
//!
//! The device's count can fall behind the server's (a refill cancelled in flight, a reinstall
//! with the same account): the server's 429 then says what is left and when the day ends, the
//! device adopts that, and asks once more for at most what is left.

use pingers_anon::{TokenCount, begin_token_issuance};
use pingers_proto::{
    PrivacyPassAllowance, PrivacyPassIssueRequest, PrivacyPassIssueResponse, SecretText,
    TokenAllowance,
};
use pingers_store::{TokenKeyId, TokenLedger, UnspentToken};
use pingers_time::UnixSeconds;

use crate::engine::blocking;
use crate::{ApiRequest, Route, SyncEngine, SyncError, WakeBudget};

/// The daily limit assumed before the server's parameters have been read (design B.4.8: 300).
pub const DEFAULT_DAILY_LIMIT: TokenAllowance = TokenAllowance::new(300);

/// Seconds in the server's issuance day (a UTC day).
const DAY_SECONDS: u64 = 86_400;

/// The status of a batch refused for the daily allowance.
const TOO_MANY_REQUESTS: u16 = 429;

/// How many tokens to keep and to fetch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenStock {
    minimum: u16,
    target: TokenCount,
}

impl TokenStock {
    /// Refill when fewer than `minimum` are left, up to `target`.
    ///
    /// # Errors
    ///
    /// [`SyncError::OutOfRange`] if `minimum` exceeds `target`.
    pub const fn new(minimum: u16, target: TokenCount) -> Result<Self, SyncError> {
        if minimum > target.get() {
            return Err(SyncError::OutOfRange("token stock minimum"));
        }
        Ok(Self { minimum, target })
    }

    /// The design's numbers: keep at least 20, fetch up to 300 (a daily batch).
    ///
    /// # Errors
    ///
    /// Never in practice; [`SyncError::Anon`] if the batch limit were below 300.
    pub fn design_default() -> Result<Self, SyncError> {
        Self::new(20, TokenCount::new(TokenCount::MAX)?)
    }

    /// Below this many tokens a refill is due.
    #[must_use]
    pub const fn minimum(self) -> u16 {
        self.minimum
    }

    /// A refill tops the stock up to this many.
    #[must_use]
    pub const fn target(self) -> TokenCount {
        self.target
    }

    /// How many to fetch when `held` are left, or `None` if no refill is due.
    #[must_use]
    pub fn shortfall(self, held: u32) -> Option<TokenCount> {
        if held >= u32::from(self.minimum) {
            return None;
        }
        let missing = u32::from(self.target.get()).saturating_sub(held);
        u16::try_from(missing)
            .ok()
            .and_then(|missing| TokenCount::new(missing).ok())
    }
}

/// The issuer key tokens are fetched against (from `GET /v1/pp/keys`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenIssuer {
    /// The issuer's public key, 49-byte compressed P-384 (a wire value).
    pub public_key: Vec<u8>,
    /// `SHA-256(public_key)`, under which the tokens are stored.
    pub key_id: TokenKeyId,
}

/// What a refill did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
#[non_exhaustive]
pub enum Refill {
    /// Enough tokens were held; nothing was fetched.
    Stocked {
        /// Tokens held.
        held: u32,
    },
    /// A batch was fetched.
    Fetched {
        /// Tokens added.
        added: u16,
        /// Tokens held now.
        held: u32,
    },
    /// A refill is due but today's allowance is spent; nothing more is asked for until the
    /// next UTC day.
    DailyLimitReached {
        /// Tokens held.
        held: u32,
        /// When the next day's allowance starts.
        resets_at: UnixSeconds,
    },
}

/// What a refill should ask for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RefillPlan {
    /// Enough is held.
    Stocked,
    /// Fetch this many: never more than today's allowance has left.
    Fetch(TokenCount),
    /// A refill is due but nothing is left today.
    DailyLimitReached,
}

/// The server's issuance day containing `now`.
pub(crate) const fn issuance_day(now: UnixSeconds) -> u64 {
    now.get() / DAY_SECONDS
}

/// When the day after `day` starts.
pub(crate) fn next_day(day: u64) -> Result<UnixSeconds, SyncError> {
    day.checked_add(1)
        .and_then(|next| next.checked_mul(DAY_SECONDS))
        .map(UnixSeconds::new)
        .ok_or(SyncError::OutOfRange("issuance day"))
}

/// Tokens the account may still be issued on `today`, by the ledger.
pub(crate) fn remaining_today(ledger: &TokenLedger, today: u64) -> u16 {
    let issued = if ledger.day == today {
        ledger.issued
    } else {
        0
    };
    ledger.daily_limit.saturating_sub(issued)
}

/// The refill for `held` tokens under `stock` and `ledger` on `today`: the shortfall, capped by
/// what is left of today's allowance.
pub(crate) fn plan_refill(
    stock: TokenStock,
    held: u32,
    ledger: &TokenLedger,
    today: u64,
) -> RefillPlan {
    let Some(shortfall) = stock.shortfall(held) else {
        return RefillPlan::Stocked;
    };
    let count = shortfall.get().min(remaining_today(ledger, today));
    match TokenCount::new(count) {
        Ok(count) => RefillPlan::Fetch(count),
        Err(_) => RefillPlan::DailyLimitReached,
    }
}

/// The ledger after `added` tokens were issued on `today`.
fn after_issuance(ledger: TokenLedger, today: u64, added: u16) -> TokenLedger {
    let issued = if ledger.day == today {
        ledger.issued
    } else {
        0
    };
    TokenLedger {
        daily_limit: ledger.daily_limit,
        day: today,
        issued: issued.saturating_add(added),
    }
}

/// The ledger after the server said what is left of `today`'s allowance.
fn after_refusal(ledger: TokenLedger, today: u64, allowance: PrivacyPassAllowance) -> TokenLedger {
    TokenLedger {
        daily_limit: ledger.daily_limit,
        day: today,
        issued: ledger
            .daily_limit
            .saturating_sub(allowance.remaining.get().min(ledger.daily_limit)),
    }
}

/// One issuance request: how many tokens, against which key, with which bearer.
struct Batch<'call> {
    issuer: &'call TokenIssuer,
    bearer: &'call SecretText,
    count: TokenCount,
}

/// What one issuance request came to.
enum Issuance {
    /// The tokens.
    Issued(Vec<UnspentToken>),
    /// 429: the batch would pass the daily allowance; nothing was issued.
    Refused(PrivacyPassAllowance),
}

impl SyncEngine {
    /// How many Privacy Pass tokens are held.
    ///
    /// # Errors
    ///
    /// [`SyncError::Store`].
    pub async fn token_count(&self) -> Result<u32, SyncError> {
        blocking(&self.database, |session| {
            Ok(session.privacy_pass_tokens().count()?)
        })
        .await
    }

    /// The token ledger: the stored one, or a fresh one with the design's limit.
    pub(crate) async fn token_ledger(&self) -> Result<TokenLedger, SyncError> {
        let today = issuance_day(self.now());
        blocking(&self.database, move |session| {
            Ok(session.token_allowance().load()?.unwrap_or(TokenLedger {
                daily_limit: DEFAULT_DAILY_LIMIT.get(),
                day: today,
                issued: 0,
            }))
        })
        .await
    }

    async fn save_ledger(&self, ledger: TokenLedger) -> Result<(), SyncError> {
        blocking(&self.database, move |session| {
            Ok(session.token_allowance().save(&ledger)?)
        })
        .await
    }

    /// Records the daily limit the server advertises (`Params.privacy_pass_daily_limit`).
    pub(crate) async fn note_daily_limit(&self, limit: TokenAllowance) -> Result<(), SyncError> {
        let mut ledger = self.token_ledger().await?;
        if ledger.daily_limit == limit.get() {
            return Ok(());
        }
        ledger.daily_limit = limit.get();
        self.save_ledger(ledger).await
    }

    /// Tops the token stock up if it is below its minimum: one `POST /v1/pp/issue` (identified,
    /// direct) for the shortfall, capped by what is left of today's allowance. A refusal for the
    /// allowance (429) is adopted and asked once more for at most what the server says is left.
    /// Call from the main app, which holds the account's bearer token.
    ///
    /// # Errors
    ///
    /// [`SyncError::Status`] if the issuer refuses for another reason; [`SyncError::Anon`] if
    /// the answer does not verify; [`SyncError::BadResponse`]; [`SyncError::BudgetExhausted`];
    /// [`SyncError::Transport`]; [`SyncError::Store`].
    pub async fn refill_tokens(
        &self,
        issuer: &TokenIssuer,
        bearer: &SecretText,
        budget: WakeBudget,
    ) -> Result<Refill, SyncError> {
        let stock = self.settings().token_stock;
        let today = issuance_day(self.now());
        let mut ledger = self.token_ledger().await?;
        // At most two requests: the planned batch, and one sized by the server's refusal.
        for _ in 0..2 {
            let held = self.token_count().await?;
            let count = match plan_refill(stock, held, &ledger, today) {
                RefillPlan::Stocked => return Ok(Refill::Stocked { held }),
                RefillPlan::DailyLimitReached => break,
                RefillPlan::Fetch(count) => count,
            };
            let batch = Batch {
                issuer,
                bearer,
                count,
            };
            match self.issue(&batch, budget).await? {
                Issuance::Issued(tokens) => {
                    let added =
                        u16::try_from(tokens.len()).map_err(|_| SyncError::OutOfRange("tokens"))?;
                    let ledger = after_issuance(ledger, today, added);
                    let key_id = issuer.key_id;
                    let held = blocking(&self.database, move |session| {
                        session.transaction(|session| {
                            let repository = session.privacy_pass_tokens();
                            repository.add(&tokens, &key_id)?;
                            session.token_allowance().save(&ledger)?;
                            Ok::<_, SyncError>(repository.count()?)
                        })
                    })
                    .await?;
                    return Ok(Refill::Fetched { added, held });
                }
                Issuance::Refused(allowance) => {
                    ledger = after_refusal(ledger, today, allowance);
                    self.save_ledger(ledger).await?;
                }
            }
        }
        Ok(Refill::DailyLimitReached {
            held: self.token_count().await?,
            resets_at: next_day(today)?,
        })
    }

    /// One `POST /v1/pp/issue`.
    async fn issue(&self, batch: &Batch<'_>, budget: WakeBudget) -> Result<Issuance, SyncError> {
        let pending =
            self.with_rng(|rng| begin_token_issuance(&batch.issuer.public_key, batch.count, rng))?;
        let body =
            PrivacyPassIssueRequest::new(pending.request().to_vec()).map_err(SyncError::Encode)?;
        let response = self
            .exchange(ApiRequest::privacy_pass_issue(&body, batch.bearer)?, budget)
            .await?;
        if response.status() == TOO_MANY_REQUESTS {
            return Ok(Issuance::Refused(response.decode()?));
        }
        let answer: PrivacyPassIssueResponse = response.success()?.decode()?;
        Ok(Issuance::Issued(
            pending
                .finish(answer.token_response())?
                .iter()
                .map(|token| UnspentToken::from_bytes(token.as_bytes().to_vec()))
                .collect(),
        ))
    }
}

/// Whether a refill error means "the bearer lapsed": fetch a fresh one and try once more.
pub(crate) const fn bearer_lapsed(error: &SyncError) -> bool {
    matches!(
        error,
        SyncError::Status {
            route: Route::PrivacyPassIssue,
            status: 401,
        }
    )
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use proptest::prelude::*;

    use super::*;

    fn stock(minimum: u16, target: u16) -> Result<TokenStock, SyncError> {
        TokenStock::new(minimum, TokenCount::new(target)?)
    }

    const TODAY: u64 = 20_406;

    fn ledger(daily_limit: u16, day: u64, issued: u16) -> TokenLedger {
        TokenLedger {
            daily_limit,
            day,
            issued,
        }
    }

    #[test]
    fn a_refill_is_due_only_below_the_minimum_and_tops_up_to_the_target() -> Result<(), SyncError> {
        let stock = stock(20, 300)?;
        assert_eq!(stock.shortfall(20), None);
        assert_eq!(stock.shortfall(400), None);
        assert_eq!(stock.shortfall(19).map(TokenCount::get), Some(281));
        assert_eq!(stock.shortfall(0).map(TokenCount::get), Some(300));
        Ok(())
    }

    #[test]
    fn the_minimum_cannot_exceed_the_target() {
        assert!(stock(301, 300).is_err());
        assert!(stock(300, 300).is_ok());
    }

    #[test]
    fn the_design_default_keeps_twenty_and_fetches_three_hundred() -> Result<(), SyncError> {
        let stock = TokenStock::design_default()?;
        assert_eq!(stock.minimum(), 20);
        assert_eq!(stock.target().get(), 300);
        Ok(())
    }

    #[test]
    fn a_second_refill_the_same_day_asks_only_for_what_is_left() -> Result<(), SyncError> {
        let stock = TokenStock::design_default()?;
        // The first batch of the day took the whole allowance: nothing is asked for.
        assert_eq!(
            plan_refill(stock, 19, &ledger(300, TODAY, 300), TODAY),
            RefillPlan::DailyLimitReached
        );
        // A smaller first batch leaves the rest.
        assert_eq!(
            plan_refill(stock, 0, &ledger(300, TODAY, 250), TODAY),
            RefillPlan::Fetch(TokenCount::new(50)?)
        );
        // A new day starts afresh.
        assert_eq!(
            plan_refill(stock, 19, &ledger(300, TODAY.saturating_sub(1), 300), TODAY),
            RefillPlan::Fetch(TokenCount::new(281)?)
        );
        assert_eq!(
            plan_refill(stock, 20, &ledger(300, TODAY, 300), TODAY),
            RefillPlan::Stocked
        );
        Ok(())
    }

    #[test]
    fn a_lower_advertised_limit_caps_the_batch() -> Result<(), SyncError> {
        let stock = TokenStock::design_default()?;
        assert_eq!(
            plan_refill(stock, 0, &ledger(100, TODAY, 0), TODAY),
            RefillPlan::Fetch(TokenCount::new(100)?)
        );
        Ok(())
    }

    #[test]
    fn a_refusal_is_adopted_as_what_was_issued_today() {
        let refused = after_refusal(
            ledger(300, TODAY.saturating_sub(1), 300),
            TODAY,
            PrivacyPassAllowance {
                remaining: TokenAllowance::new(19),
                resets_at: UnixSeconds::new(0),
            },
        );
        assert_eq!(refused, ledger(300, TODAY, 281));
        assert_eq!(remaining_today(&refused, TODAY), 19);
        // A server claiming more than its own limit is held to the limit.
        let generous = after_refusal(
            ledger(300, TODAY, 0),
            TODAY,
            PrivacyPassAllowance {
                remaining: TokenAllowance::new(500),
                resets_at: UnixSeconds::new(0),
            },
        );
        assert_eq!(remaining_today(&generous, TODAY), 300);
    }

    #[test]
    fn issuance_counts_today_and_restarts_on_a_new_day() {
        assert_eq!(
            after_issuance(ledger(300, TODAY, 100), TODAY, 50),
            ledger(300, TODAY, 150)
        );
        assert_eq!(
            after_issuance(ledger(300, TODAY.saturating_sub(1), 300), TODAY, 50),
            ledger(300, TODAY, 50)
        );
    }

    #[test]
    fn the_day_ends_at_utc_midnight() -> Result<(), SyncError> {
        assert_eq!(issuance_day(UnixSeconds::new(86_399)), 0);
        assert_eq!(issuance_day(UnixSeconds::new(86_400)), 1);
        assert_eq!(next_day(1)?, UnixSeconds::new(172_800));
        Ok(())
    }

    proptest! {
        /// Whatever the stock, the ledger and the day, a refill and everything already issued
        /// today together never pass the daily limit.
        #[test]
        fn a_refill_never_exceeds_the_daily_limit(
            minimum in 0_u16..=300,
            target in 1_u16..=300,
            held in 0_u32..400,
            limit in 0_u16..=400,
            issued in 0_u16..=400,
            days_ago in 0_u64..3,
        ) {
            let Ok(stock) = stock(minimum.min(target), target) else {
                return Err(TestCaseError::fail("stock"));
            };
            let ledger = ledger(limit, TODAY.saturating_sub(days_ago), issued);
            let already = if days_ago == 0 { issued } else { 0 };
            if let RefillPlan::Fetch(count) = plan_refill(stock, held, &ledger, TODAY) {
                prop_assert!(
                    u32::from(already).saturating_add(u32::from(count.get())) <= u32::from(limit)
                );
                prop_assert!(u32::from(count.get()).saturating_add(held) <= u32::from(target));
            }
        }
    }
}
