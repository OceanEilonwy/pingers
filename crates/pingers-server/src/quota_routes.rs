// SPDX-License-Identifier: AGPL-3.0-only
//! The account, membership tree, Privacy Pass and parameter routes: each one call into
//! pingers-quota. Blocking (SQLite): `dispatch.rs` runs them on the blocking pool.
//!
//! Wire encodings: `?after=` on `GET /v1/rln/leaves` is a decimal leaf position (0 when
//! absent); Acct requests carry `Authorization: Bearer <64 hex>` from `POST /v1/auth`.

use axum::http::StatusCode;
use pingers_anon::IdCommitment;
use pingers_board_store::PageSize;
use pingers_crypto::BearerToken;
use pingers_ohttp::BinaryRequest;
use pingers_proto::{
    AccountId, AnonymousCredential, AuthRequest, AuthResponse, CoverReadRatio, CustomPushRelays,
    MembershipIndex, MembershipLeafRequest, Params, PrivacyPassIssueRequest,
    PrivacyPassIssueResponse, PrivacyPassKey, PrivacyPassKeys, RegisterRequest, RegisterResponse,
    SecretText, Slashing, TokenAllowance,
};
use pingers_quota::{DAILY_TOKEN_LIMIT, QuotaError};
use pingers_time::Clock as _;
use zeroize::Zeroizing;

use crate::credential::AUTHORIZATION;
use crate::dispatch::Reply;
use crate::wire::{decode, encoded, header, hex_array, query};
use crate::{MembershipProofs, Shared};

/// The `Authorization` scheme of the Acct credential.
const BEARER_SCHEME: &str = "Bearer ";

fn status_of(error: &QuotaError) -> StatusCode {
    match error {
        QuotaError::DailyLimit(_) | QuotaError::SignupLimit | QuotaError::LeafLimit => {
            StatusCode::TOO_MANY_REQUESTS
        }
        QuotaError::BadSignature | QuotaError::RegistrationCode => StatusCode::FORBIDDEN,
        QuotaError::Anon(
            pingers_anon::AnonError::NotAFieldElement | pingers_anon::AnonError::BadIssuance,
        ) => StatusCode::UNPROCESSABLE_ENTITY,
        QuotaError::Anon(pingers_anon::AnonError::TreeFull) => StatusCode::INSUFFICIENT_STORAGE,
        QuotaError::Anon(_)
        | QuotaError::StaleBucket
        | QuotaError::NullifierReused
        | QuotaError::DoubleSpend
        | QuotaError::TimeOutOfRange
        | QuotaError::RootMismatch
        | QuotaError::CorruptField { .. }
        | QuotaError::Poisoned
        | QuotaError::Sqlite(_)
        | _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// The answer to a refusal: a status, and for the daily limit what is left of the allowance.
fn refused(error: &QuotaError) -> Reply {
    if let QuotaError::DailyLimit(allowance) = error {
        return encoded(status_of(error), allowance);
    }
    Reply::status(status_of(error))
}

/// The bearer in an `Authorization: Bearer <64 hex>` value.
fn bearer_of(value: &str) -> Option<BearerToken> {
    hex_array::<{ BearerToken::LENGTH }>(value.strip_prefix(BEARER_SCHEME)?)
        .map(|bytes| BearerToken::from_bytes(Zeroizing::new(bytes)))
}

/// The account a request's bearer names: 401 without a valid one.
fn account(shared: &Shared, request: &BinaryRequest) -> Result<AccountId, Reply> {
    let unauthorized = || Reply::status(StatusCode::UNAUTHORIZED);
    let bearer = header(request, AUTHORIZATION)
        .and_then(bearer_of)
        .ok_or_else(unauthorized)?;
    shared
        .quota
        .accounts
        .account_of(&bearer)
        .map_err(|error| refused(&error))?
        .ok_or_else(unauthorized)
}

/// `GET /v1/params`.
#[expect(
    clippy::unnecessary_wraps,
    reason = "every blocking route handler has the same signature"
)]
pub(crate) fn params(shared: &Shared, _request: &BinaryRequest) -> Result<Reply, Reply> {
    Ok(encoded(
        StatusCode::OK,
        &Params {
            anonymous_credentials: match shared.params.membership_proofs {
                MembershipProofs::On => vec![
                    AnonymousCredential::MembershipProof,
                    AnonymousCredential::PrivacyPass,
                ],
                MembershipProofs::Off => vec![AnonymousCredential::PrivacyPass],
            },
            rln_slashing: Slashing::Off,
            cover_read_ratio: CoverReadRatio::ALWAYS,
            board_page_records: u32::try_from(PageSize::MAX.get()).unwrap_or(u32::MAX),
            ohttp_relays: shared.params.ohttp_relays.clone(),
            push_relays: shared.params.push_relays.clone(),
            custom_push_relays: CustomPushRelays::NotAllowed,
            server_operator: shared.params.server_operator.clone(),
            server_time: shared.clock.now(),
            map_host: shared.params.map_host.clone(),
            privacy_pass_daily_limit: TokenAllowance::new(DAILY_TOKEN_LIMIT),
        },
    ))
}

/// `POST /v1/accounts`.
pub(crate) fn create_account(shared: &Shared, request: &BinaryRequest) -> Result<Reply, Reply> {
    let registration = decode::<RegisterRequest>(&request.body)?;
    let account_id = shared
        .quota
        .accounts
        .register(&registration)
        .map_err(|error| refused(&error))?;
    Ok(encoded(StatusCode::OK, &RegisterResponse { account_id }))
}

/// `POST /v1/auth`.
pub(crate) fn auth(shared: &Shared, request: &BinaryRequest) -> Result<Reply, Reply> {
    let auth = decode::<AuthRequest>(&request.body)?;
    let now = shared.clock.now();
    let bearer = shared.with_rng(BearerToken::generate);
    let _expires_at = shared
        .quota
        .accounts
        .authenticate(&auth, &bearer)
        .map_err(|error| refused(&error))?;
    Ok(encoded(
        StatusCode::OK,
        &AuthResponse {
            bearer: SecretText::new(hex::encode(bearer.expose_bytes())),
            server_time: now,
        },
    ))
}

/// `DELETE /v1/accounts/me`: the leaf is zeroed at the next root, and the account forgotten.
pub(crate) fn delete_account(shared: &Shared, request: &BinaryRequest) -> Result<Reply, Reply> {
    let account = account(shared, request)?;
    shared
        .quota
        .membership
        .remove(&account)
        .map_err(|error| refused(&error))?;
    shared
        .quota
        .accounts
        .delete(&account)
        .map_err(|error| refused(&error))?;
    Ok(Reply::status(StatusCode::NO_CONTENT))
}

/// `POST /v1/rln/leaf` (Acct).
pub(crate) fn rln_leaf(shared: &Shared, request: &BinaryRequest) -> Result<Reply, Reply> {
    let account = account(shared, request)?;
    let leaf = decode::<MembershipLeafRequest>(&request.body)?;
    let commitment = IdCommitment::from_bytes(*leaf.id_commitment.get());
    let registered = shared
        .quota
        .membership
        .register(account, &commitment)
        .map_err(|error| refused(&error))?;
    Ok(encoded(StatusCode::OK, &registered))
}

/// `GET /v1/rln/leaves?after=n`.
pub(crate) fn rln_leaves(shared: &Shared, request: &BinaryRequest) -> Result<Reply, Reply> {
    let after = match query(&request.path, "after") {
        None => MembershipIndex::new(0),
        Some(text) => MembershipIndex::new(
            text.parse()
                .map_err(|_| Reply::status(StatusCode::BAD_REQUEST))?,
        ),
    };
    let leaves = shared
        .quota
        .membership
        .leaves(after)
        .map_err(|error| refused(&error))?;
    Ok(encoded(StatusCode::OK, &leaves))
}

/// `POST /v1/pp/issue` (Acct): at most [`DAILY_TOKEN_LIMIT`] tokens per account per day. A
/// batch that would exceed it is refused whole with `429` and a
/// [`pingers_proto::PrivacyPassAllowance`] body: what the account may still be issued today,
/// and when the next day starts.
pub(crate) fn issue(shared: &Shared, request: &BinaryRequest) -> Result<Reply, Reply> {
    let account = account(shared, request)?;
    let batch = decode::<PrivacyPassIssueRequest>(&request.body)?;
    let issued = shared
        .quota
        .privacy_pass
        .issue(account, batch.token_request(), &mut shared.forked_rng())
        .map_err(|error| refused(&error))?;
    let response = PrivacyPassIssueResponse::new(issued.response)
        .map_err(|_| Reply::status(StatusCode::INTERNAL_SERVER_ERROR))?;
    Ok(encoded(StatusCode::OK, &response))
}

/// `GET /v1/pp/keys`.
pub(crate) fn keys(shared: &Shared, _request: &BinaryRequest) -> Result<Reply, Reply> {
    let keys = shared
        .quota
        .privacy_pass
        .public_keys()
        .map_err(|error| refused(&error))?;
    Ok(encoded(
        StatusCode::OK,
        &PrivacyPassKeys {
            keys: keys
                .into_iter()
                .map(|key| PrivacyPassKey {
                    key_id: key.key_id,
                    public_key: key.public_key,
                    issue_until: key.issue_until,
                    redeem_until: key.redeem_until,
                })
                .collect(),
        },
    ))
}
