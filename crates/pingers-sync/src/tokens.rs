// SPDX-License-Identifier: AGPL-3.0-only
//! The Privacy Pass stock (design B.4.8): the main app fetches tokens in batches over the
//! authenticated API and keeps a minimum in the shared store for extensions and fallback.

use pingers_anon::{TokenCount, begin_token_issuance};
use pingers_proto::{PrivacyPassIssueRequest, PrivacyPassIssueResponse, SecretText};
use pingers_store::{TokenKeyId, UnspentToken};

use crate::engine::blocking;
use crate::{ApiRequest, SyncEngine, SyncError, WakeBudget};

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
}

impl SyncEngine {
    /// How many Privacy Pass tokens are held.
    ///
    /// # Errors
    ///
    /// [`SyncError::Store`].
    pub async fn token_count(&self) -> Result<u32, SyncError> {
        blocking(&self.session, |session| {
            Ok(session.privacy_pass_tokens().count()?)
        })
        .await
    }

    /// Tops the token stock up if it is below its minimum: one `POST /v1/pp/issue`
    /// (identified, direct) for the whole shortfall. Call from the main app, which holds the
    /// account's bearer token.
    ///
    /// # Errors
    ///
    /// [`SyncError::Status`] if the issuer refuses; [`SyncError::Anon`] if the answer does
    /// not verify; [`SyncError::BudgetExhausted`]; [`SyncError::Transport`];
    /// [`SyncError::Store`].
    pub async fn refill_tokens(
        &self,
        issuer: &TokenIssuer,
        bearer: &SecretText,
        budget: WakeBudget,
    ) -> Result<Refill, SyncError> {
        let held = self.token_count().await?;
        let Some(count) = self.settings().token_stock.shortfall(held) else {
            return Ok(Refill::Stocked { held });
        };
        let pending = self.with_rng(|rng| begin_token_issuance(&issuer.public_key, count, rng))?;
        let body =
            PrivacyPassIssueRequest::new(pending.request().to_vec()).map_err(SyncError::Encode)?;
        let response = self
            .exchange(ApiRequest::privacy_pass_issue(&body, bearer)?, budget)
            .await?
            .success()?;
        let answer: PrivacyPassIssueResponse = response.decode()?;
        let tokens: Vec<UnspentToken> = pending
            .finish(answer.token_response())?
            .iter()
            .map(|token| UnspentToken::from_bytes(token.as_bytes().to_vec()))
            .collect();
        let added = u16::try_from(tokens.len()).map_err(|_| SyncError::OutOfRange("tokens"))?;
        let key_id = issuer.key_id;
        let held = blocking(&self.session, move |session| {
            let repository = session.privacy_pass_tokens();
            repository.add(&tokens, &key_id)?;
            Ok(repository.count()?)
        })
        .await?;
        Ok(Refill::Fetched { added, held })
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    fn stock(minimum: u16, target: u16) -> Result<TokenStock, SyncError> {
        TokenStock::new(minimum, TokenCount::new(target)?)
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
    fn the_minimum_cannot_exceed_the_target() -> Result<(), SyncError> {
        assert!(stock(301, 300).is_err());
        assert!(stock(300, 300).is_ok());
        Ok(())
    }

    #[test]
    fn the_design_default_keeps_twenty_and_fetches_three_hundred() -> Result<(), SyncError> {
        let stock = TokenStock::design_default()?;
        assert_eq!(stock.minimum(), 20);
        assert_eq!(stock.target().get(), 300);
        Ok(())
    }
}
