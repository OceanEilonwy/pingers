// SPDX-License-Identifier: AGPL-3.0-only
//! The account and the credentials it buys (design B.1, B.4.8, D): the device's Ed25519 key,
//! its registration (`POST /v1/accounts`, with the operator's registration code if any), the
//! bearer (`POST /v1/auth` over the current bucket), the membership leaf (`POST /v1/rln/leaf`)
//! and the Privacy Pass stock (`POST /v1/pp/issue`). All of these are identified requests, sent
//! direct; the issuer's key list is public and fetched through the relay.
//!
//! Every step is recorded in the store as it completes, so [`SyncEngine::prepare_credentials`]
//! is safe to call on every wake and after any crash: once registered, authenticated within the
//! hour, with a leaf and a full stock, it makes no request at all.
//!
//! Refusals never loop. A 403 to the registration is recorded with the code that was refused,
//! and registration is not tried again until another code is offered
//! ([`SyncEngine::offer_registration_code`]). A 401 to a bearer request means the bearer is no
//! longer accepted: it is dropped, a fresh one fetched, and the request tried once more. A 403
//! to `POST /v1/auth` means the server does not know the account (an operator deleted it, or a
//! server whose quota store is in memory restarted): the device registers again once, with the
//! code it registered with.
//!
//! The issuer key is not pinned at build time (it rotates weekly). The design has no Party-state
//! hash for it, so the checks are: the list comes through the relay, where the server cannot tell
//! devices apart and so cannot hand one device a key of its own; every key must be named by its
//! own `SHA-256` (RFC 9578 `token_key_id`); and the identified issuance must verify under that
//! anonymously fetched key (the batch DLEQ proof in pingers-anon), so a server that issued this
//! account tokens under a different, tagging key would fail the refill.

use core::time::Duration;

use pingers_anon::{RlnSecret, token_key_id};
use pingers_crypto::{AccountSigningKey, account_id, sign_auth_challenge, sign_registration};
use pingers_proto::{
    AuthRequest, AuthResponse, MembershipLeafRequest, MembershipLeafResponse, PrivacyPassKeys,
    Redacted, RegisterRequest, RegisterResponse, SecretText,
};
use pingers_store::{
    AccountBearer, AccountRegistration, Identity, RlnIdentity, TokenKeyId, TreePosition,
};
use pingers_time::UnixSeconds;
use subtle::ConstantTimeEq as _;

use crate::credential::readiness;
use crate::engine::blocking;
use crate::tokens::{RefillPlan, bearer_lapsed, issuance_day, next_day, plan_refill};
use crate::{
    ApiRequest, MembershipRefresh, ProcessRole, Refill, Response, RlnReadiness, Route, SyncEngine,
    SyncError, TokenIssuer, WakeBudget,
};

/// How long the server accepts a bearer (design D: one hour).
pub const BEARER_LIFETIME: Duration = Duration::from_hours(1);

/// A bearer is replaced this long before it would expire, so a request never carries one that
/// lapses in flight or by clock difference.
const BEARER_MARGIN: Duration = Duration::from_mins(5);

/// The status a request with a missing or lapsed bearer gets.
const UNAUTHORIZED: u16 = 401;
/// The status of a refused registration or challenge.
const FORBIDDEN: u16 = 403;

/// Where an offered registration code comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CodeSource {
    /// The app's build configuration (a debug build's `PINGERS_DEV_REGISTRATION_CODE`).
    Build,
    /// An invite link the user opened.
    Invite,
}

/// This device's membership leaf.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub enum LeafStanding {
    /// Registered earlier; nothing was sent.
    Held,
    /// Registered now.
    Registered,
}

/// What [`SyncEngine::prepare_credentials`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub struct Prepared {
    /// The Privacy Pass stock.
    pub tokens: Refill,
    /// The membership leaf.
    pub leaf: LeafStanding,
    /// The membership tree refresh, if one was due (main app only).
    pub membership: Option<MembershipRefresh>,
}

/// What a key list says to do.
struct IssuerChoice {
    /// The key to fetch tokens against.
    issuer: TokenIssuer,
    /// The keys whose tokens are still accepted; tokens of any other key are deleted.
    redeemable: Vec<TokenKeyId>,
}

/// The issuing key of `keys` at `now` (the newest whose issuing time has not passed), after
/// checking that every key is named by its own hash.
fn choose_issuer(keys: &PrivacyPassKeys, now: UnixSeconds) -> Result<IssuerChoice, SyncError> {
    if keys
        .keys
        .iter()
        .any(|key| token_key_id(&key.public_key) != key.key_id)
    {
        return Err(SyncError::IssuerKeys);
    }
    let issuing = keys
        .keys
        .iter()
        .rev()
        .find(|key| now < key.issue_until)
        .ok_or(SyncError::IssuerKeys)?;
    Ok(IssuerChoice {
        issuer: TokenIssuer {
            public_key: issuing.public_key.clone(),
            key_id: TokenKeyId::from_bytes(issuing.key_id),
        },
        redeemable: keys
            .keys
            .iter()
            .filter(|key| now < key.redeem_until)
            .map(|key| TokenKeyId::from_bytes(key.key_id))
            .collect(),
    })
}

/// Whether a status means "fetch a fresh bearer and try once more".
const fn needs_fresh_bearer(response: &Response) -> bool {
    response.status() == UNAUTHORIZED
}

impl SyncEngine {
    /// Offers the operator's registration code for the account's registration. An account
    /// already registered keeps its standing, and a code already refused is not taken again.
    /// Otherwise a code from an invite link replaces whatever waits; a code from the build
    /// configuration is taken only when no other code waits, so a code the user brought in
    /// later is not overwritten at the next start. An empty code offers nothing.
    ///
    /// # Errors
    ///
    /// [`SyncError::Store`].
    pub async fn offer_registration_code(
        &self,
        code: SecretText,
        source: CodeSource,
    ) -> Result<(), SyncError> {
        if code.expose().is_empty() {
            return Ok(());
        }
        blocking(&self.database, move |session| {
            let accounts = session.account();
            let offered = match accounts.registration()? {
                AccountRegistration::Registered { .. } => return Ok(()),
                AccountRegistration::Refused {
                    code: Some(refused),
                } if bool::from(refused.ct_eq(&code)) => return Ok(()),
                AccountRegistration::Pending { code: Some(_) } if source == CodeSource::Build => {
                    return Ok(());
                }
                AccountRegistration::Pending { .. } | AccountRegistration::Refused { .. } => {
                    AccountRegistration::Pending { code: Some(code) }
                }
            };
            Ok(accounts.set_registration(&offered)?)
        })
        .await
    }

    /// The registration code this account was registered with, to pass on in invite links;
    /// `None` before registration or for a server without codes.
    ///
    /// # Errors
    ///
    /// [`SyncError::Store`].
    pub async fn shared_registration_code(&self) -> Result<Option<SecretText>, SyncError> {
        blocking(&self.database, |session| {
            Ok(match session.account().registration()? {
                AccountRegistration::Registered { code } => code,
                AccountRegistration::Pending { .. } | AccountRegistration::Refused { .. } => None,
            })
        })
        .await
    }

    /// Makes sure every credential is in place: the account registered, the Privacy Pass stock
    /// above its minimum (within today's allowance), its membership leaf registered, and, in
    /// the main app, the membership tree refreshed when its hourly root has passed and the
    /// prover self-test started if it never ran. Each step that is already done costs one store
    /// read and no request. Call it before credentialed requests and on wakes; a wake runs it
    /// inside its budget.
    ///
    /// Not cancellation-safe in one respect only: tokens issued in a cancelled refill are lost
    /// (they count against the day's allowance, and the server's next 429 says so). Every other
    /// step is redone safely.
    ///
    /// # Errors
    ///
    /// [`SyncError::RegistrationRefused`]; [`SyncError::NoGatewayKey`]; [`SyncError::IssuerKeys`];
    /// [`SyncError::TreeMismatch`]; [`SyncError::Status`]; as [`SyncEngine::send`].
    pub async fn prepare_credentials(&self, budget: WakeBudget) -> Result<Prepared, SyncError> {
        self.ensure_registered(budget).await?;
        let tokens = self.stock_tokens(budget).await?;
        // The tree first: a fresh copy is what shows a removed leaf, which is then replaced.
        let membership = if self.settings().role == ProcessRole::MainApp {
            let _: crate::SelfTestStart = self.start_prover_self_test().await?;
            self.ensure_membership_fresh(budget).await?
        } else {
            None
        };
        let leaf = self.ensure_membership_leaf(budget).await?;
        Ok(Prepared {
            tokens,
            leaf,
            membership,
        })
    }

    /// The account, made on first use: a fresh Ed25519 key for the engine's server, recorded
    /// with the gateway key config hash the relay served (fetched and checked against the
    /// transport's pin first).
    async fn ensure_identity(&self, budget: WakeBudget) -> Result<(), SyncError> {
        let held = blocking(&self.database, |session| {
            Ok(session.identity().load()?.is_some())
        })
        .await?;
        if held {
            return Ok(());
        }
        self.check_relay(budget).await?;
        let server_pin = self
            .transport
            .gateway_key_hash()
            .ok_or(SyncError::NoGatewayKey)?;
        let signing_key = self.with_rng(AccountSigningKey::generate);
        let server = self.settings().server;
        blocking(&self.database, move |session| {
            session.transaction(|session| {
                // Another process may have made it meanwhile; keep the first.
                if session.identity().load()?.is_none() {
                    session.identity().save(&Identity {
                        account: account_id(&signing_key.public_key()),
                        signing_key,
                        server,
                        server_pin,
                    })?;
                    session.account().forget_bearer()?;
                }
                Ok::<_, SyncError>(())
            })
        })
        .await
    }

    /// Registers the account unless it already is.
    ///
    /// # Errors
    ///
    /// [`SyncError::RegistrationRefused`] if the server refused it, now or before with the
    /// code on offer; as [`SyncEngine::prepare_credentials`].
    pub async fn ensure_registered(&self, budget: WakeBudget) -> Result<(), SyncError> {
        self.ensure_identity(budget).await?;
        let registration = blocking(&self.database, |session| {
            Ok(session.account().registration()?)
        })
        .await?;
        match registration {
            AccountRegistration::Registered { .. } => Ok(()),
            AccountRegistration::Refused { .. } => Err(SyncError::RegistrationRefused),
            AccountRegistration::Pending { code } => self.register(code, budget).await,
        }
    }

    /// `POST /v1/accounts` with `code`, recording the answer.
    async fn register(
        &self,
        code: Option<SecretText>,
        budget: WakeBudget,
    ) -> Result<(), SyncError> {
        let server = self.settings().server;
        let sent_code = code.clone();
        let (request, account) = blocking(&self.database, move |session| {
            let identity = session.identity().load()?.ok_or(SyncError::Internal)?;
            let request = RegisterRequest {
                signature_key: *identity.signing_key.public_key().as_bytes(),
                proof: *sign_registration(&identity.signing_key, &server)?.as_bytes(),
                registration_code: sent_code,
            };
            Ok((ApiRequest::create_account(&request)?, identity.account))
        })
        .await?;
        let response = self.exchange(request, budget).await?;
        if response.status() == FORBIDDEN {
            blocking(&self.database, move |session| {
                session.account().forget_bearer()?;
                Ok(session
                    .account()
                    .set_registration(&AccountRegistration::Refused { code })?)
            })
            .await?;
            return Err(SyncError::RegistrationRefused);
        }
        let registered: RegisterResponse = response.success()?.decode()?;
        if registered.account_id != account {
            return Err(SyncError::BadResponse {
                route: Route::CreateAccount,
                source: pingers_proto::ProtoError::OutOfRange {
                    field: "account id",
                },
            });
        }
        blocking(&self.database, move |session| {
            Ok(session
                .account()
                .set_registration(&AccountRegistration::Registered { code })?)
        })
        .await
    }

    /// A bearer the server accepts for a while yet: the stored one, or a fresh one.
    async fn bearer(&self, budget: WakeBudget) -> Result<SecretText, SyncError> {
        let now = self.now();
        let stored = blocking(&self.database, |session| Ok(session.account().bearer()?)).await?;
        if let Some(stored) = stored
            && now < stored.expires_at
        {
            return Ok(stored.bearer);
        }
        self.authenticate(budget).await
    }

    /// `POST /v1/auth` over the current bucket. If the server no longer knows the account, it
    /// registers again (once) and tries once more.
    async fn authenticate(&self, budget: WakeBudget) -> Result<SecretText, SyncError> {
        let response = self.auth_exchange(budget).await?;
        let response = if response.status() == FORBIDDEN {
            blocking(&self.database, |session| {
                let accounts = session.account();
                if let AccountRegistration::Registered { code } = accounts.registration()? {
                    accounts.set_registration(&AccountRegistration::Pending { code })?;
                }
                Ok(())
            })
            .await?;
            self.ensure_registered(budget).await?;
            self.auth_exchange(budget).await?
        } else {
            response
        };
        let answer: AuthResponse = response.success()?.decode()?;
        let _status = self.observe_server_time(answer.server_time);
        let expires_at = self
            .now()
            .checked_add(BEARER_LIFETIME.saturating_sub(BEARER_MARGIN))
            .ok_or(SyncError::OutOfRange("bearer expiry"))?;
        let bearer = answer.bearer;
        let stored = AccountBearer {
            bearer: bearer.clone(),
            expires_at,
        };
        blocking(&self.database, move |session| {
            Ok(session.account().set_bearer(&stored)?)
        })
        .await?;
        Ok(bearer)
    }

    async fn auth_exchange(&self, budget: WakeBudget) -> Result<Response, SyncError> {
        let server = self.settings().server;
        let bucket = self.now().bucket();
        let request = blocking(&self.database, move |session| {
            let identity = session.identity().load()?.ok_or(SyncError::Internal)?;
            ApiRequest::auth(&AuthRequest {
                account_id: identity.account,
                nonce_signature: *sign_auth_challenge(&identity.signing_key, &server, bucket)?
                    .as_bytes(),
            })
        })
        .await?;
        self.exchange(request, budget).await
    }

    /// Sends the identified request `build` makes with a bearer; on a 401 drops the bearer,
    /// fetches a fresh one and sends once more. The answer is returned as is otherwise.
    async fn with_bearer(
        &self,
        budget: WakeBudget,
        build: impl Fn(&SecretText) -> Result<ApiRequest, SyncError>,
    ) -> Result<Response, SyncError> {
        let response = self
            .exchange(build(&self.bearer(budget).await?)?, budget)
            .await?;
        if !needs_fresh_bearer(&response) {
            return Ok(response);
        }
        blocking(&self.database, |session| {
            Ok(session.account().forget_bearer()?)
        })
        .await?;
        let bearer = self.authenticate(budget).await?;
        self.exchange(build(&bearer)?, budget).await
    }

    /// Registers this device's membership leaf unless it has one (design B.4.8): a fresh RLN
    /// secret, kept only once the server has taken its commitment. A crash in between leaves no
    /// identity, and the next try registers a new leaf, which replaces the lost one at the
    /// server. A leaf a fresh tree copy no longer holds (zeroed: the server forgot the account,
    /// or replaced the leaf) is replaced the same way; the old secret is never reused.
    ///
    /// # Errors
    ///
    /// [`SyncError::Status`]; as [`SyncEngine::prepare_credentials`].
    pub async fn ensure_membership_leaf(
        &self,
        budget: WakeBudget,
    ) -> Result<LeafStanding, SyncError> {
        let now = self.now();
        let held = blocking(&self.database, move |session| {
            Ok(match session.rln_identity().load()? {
                None => false,
                Some(identity) => {
                    readiness(
                        Some(&identity),
                        session.rln_tree_state().load()?.as_ref(),
                        now,
                    ) != RlnReadiness::LeafRemoved
                }
            })
        })
        .await?;
        if held {
            return Ok(LeafStanding::Held);
        }
        self.ensure_registered(budget).await?;
        let secret = self.with_rng(RlnSecret::generate)?;
        let body = MembershipLeafRequest {
            id_commitment: Redacted::new(*secret.id_commitment()?.as_bytes()),
        };
        let leaf: MembershipLeafResponse = self
            .with_bearer(budget, |bearer| ApiRequest::rln_leaf(&body, bearer))
            .await?
            .success()?
            .decode()?;
        let identity = RlnIdentity {
            secret,
            leaf: TreePosition::new(leaf.index.get()),
            live_from: leaf.live_from,
        };
        blocking(&self.database, move |session| {
            Ok(session.rln_identity().save(&identity)?)
        })
        .await?;
        Ok(LeafStanding::Registered)
    }

    /// The key to fetch tokens against: the issuing key of `GET /v1/pp/keys`, fetched through
    /// the relay. Tokens of keys the server no longer lists are deleted (they would be refused).
    ///
    /// # Errors
    ///
    /// [`SyncError::IssuerKeys`] if a key is not named by its own hash or none issues now;
    /// [`SyncError::Status`]; as [`SyncEngine::send`].
    pub async fn issuer_key(&self, budget: WakeBudget) -> Result<TokenIssuer, SyncError> {
        let keys: PrivacyPassKeys = self
            .exchange(ApiRequest::privacy_pass_keys(), budget)
            .await?
            .success()?
            .decode()?;
        let IssuerChoice { issuer, redeemable } = choose_issuer(&keys, self.now())?;
        blocking(&self.database, move |session| {
            Ok(session.privacy_pass_tokens().retain_keys(&redeemable)?)
        })
        .await?;
        Ok(issuer)
    }

    /// Tops the Privacy Pass stock up if it is below its minimum: the issuer key through the
    /// relay, a bearer, and one `POST /v1/pp/issue` (once more with a fresh bearer after a 401).
    /// A full stock, or one already topped up with all of today's allowance, costs two store
    /// reads and no request.
    ///
    /// # Errors
    ///
    /// [`SyncError::Status`] if the issuer refuses (a spent daily allowance is
    /// [`Refill::DailyLimitReached`], not an error); as [`SyncEngine::issuer_key`] and
    /// [`SyncEngine::refill_tokens`].
    pub async fn stock_tokens(&self, budget: WakeBudget) -> Result<Refill, SyncError> {
        let held = self.token_count().await?;
        let today = issuance_day(self.now());
        let ledger = self.token_ledger().await?;
        match plan_refill(self.settings().token_stock, held, &ledger, today) {
            RefillPlan::Stocked => return Ok(Refill::Stocked { held }),
            RefillPlan::DailyLimitReached => {
                return Ok(Refill::DailyLimitReached {
                    held,
                    resets_at: next_day(today)?,
                });
            }
            RefillPlan::Fetch(_) => {}
        }
        self.ensure_registered(budget).await?;
        let issuer = self.issuer_key(budget).await?;
        let bearer = self.bearer(budget).await?;
        match self.refill_tokens(&issuer, &bearer, budget).await {
            Err(error) if bearer_lapsed(&error) => {
                blocking(&self.database, |session| {
                    Ok(session.account().forget_bearer()?)
                })
                .await?;
                let bearer = self.authenticate(budget).await?;
                self.refill_tokens(&issuer, &bearer, budget).await
            }
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_proto::PrivacyPassKey;

    use super::*;

    const NOW: UnixSeconds = UnixSeconds::new(1_000);

    fn key(fill: u8, issue_until: u64, redeem_until: u64) -> PrivacyPassKey {
        let public_key = vec![fill; 49];
        PrivacyPassKey {
            key_id: token_key_id(&public_key),
            public_key,
            issue_until: UnixSeconds::new(issue_until),
            redeem_until: UnixSeconds::new(redeem_until),
        }
    }

    fn id(fill: u8) -> TokenKeyId {
        TokenKeyId::from_bytes(token_key_id(&[fill; 49]))
    }

    #[test]
    fn the_newest_key_still_issuing_is_chosen_and_retired_keys_are_dropped() -> Result<(), SyncError>
    {
        let keys = PrivacyPassKeys {
            keys: vec![
                key(1, 500, 900),
                key(2, 1_200, 2_000),
                key(3, NOW.get(), 3_000),
                // Its tokens stop being accepted this very second.
                key(4, 500, NOW.get()),
            ],
        };
        let choice = choose_issuer(&keys, NOW)?;
        assert_eq!(choice.issuer.key_id, id(2));
        assert_eq!(choice.issuer.public_key, vec![2; 49]);
        assert_eq!(choice.redeemable, [id(2), id(3)]);
        Ok(())
    }

    #[test]
    fn a_key_named_by_another_hash_spoils_the_list() {
        let mut forged = key(2, 1_200, 2_000);
        forged.key_id = token_key_id(&[9; 49]);
        let keys = PrivacyPassKeys {
            keys: vec![key(1, 1_100, 2_000), forged],
        };
        assert!(matches!(
            choose_issuer(&keys, NOW),
            Err(SyncError::IssuerKeys)
        ));
    }

    #[test]
    fn a_list_with_no_key_issuing_now_is_refused() {
        let keys = PrivacyPassKeys {
            keys: vec![key(1, NOW.get(), 2_000)],
        };
        assert!(matches!(
            choose_issuer(&keys, NOW),
            Err(SyncError::IssuerKeys)
        ));
        assert!(matches!(
            choose_issuer(&PrivacyPassKeys { keys: Vec::new() }, NOW),
            Err(SyncError::IssuerKeys)
        ));
    }
}
