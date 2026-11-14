// SPDX-License-Identifier: AGPL-3.0-only
//! The credentials of design D, checked before a route's crate is called: Anon (an RLN proof
//! or a Privacy Pass token, admitted by pingers-quota), Grp (a signature by the group's
//! current log-auth key, verified with pingers-crypto against pingers-delivery's key), and
//! group sessions.
//!
//! Wire encodings (design D leaves the header syntax open; pingers-sync must match):
//!
//! - `Authorization: PingersRln <base64url(RlnAuth)>`, the 265-byte layout; its `x` is
//!   pingers-anon's `request_digest(method, path, body)` with the path as sent, query
//!   included. The proof's scope must be the route's: Board for `POST /v1/board/sync`,
//!   Mailbox for `POST /v1/mailboxes` and `POST /v1/mailboxes/{id}/msgs`, Group for the
//!   group routes.
//! - `Authorization: PrivateToken token="<base64url(token)>"` (RFC 9577); spent once, on any
//!   route that takes an Anon credential.
//! - Grp: `pingers-group-time: <unix seconds>` and `pingers-group-signature: <hex>`, the
//!   Ed25519 signature of pingers-crypto's `GroupRequest { method, path, body, timestamp }`
//!   (for the live channel's head the body is empty). The time must be within
//!   [`GROUP_CLOCK_SKEW`] of the server's clock (design F.1: a device uses its own clock
//!   unchanged up to half a bucket off).
//! - Which group requests also need an Anon credential (pingers-sync's contract): the writes
//!   (create, commit, post, put subscription). Reads, deletes and the live channel carry
//!   only the signature, which already proves the sender holds the epoch's key.
//! - Sessions: design D's "one Anon credential per group per bucket buys a session cookie for
//!   that group". A group write that spends an Anon credential (and `POST /v1/groups`) is
//!   answered with `pingers-group-session: <hex>`; presenting it on later writes to that
//!   group, until the end of the bucket and for at most [`SESSION_WRITES`] writes, stands in
//!   for the credential (past that, the write needs a credential again, which starts a new
//!   session). Optional: a client may send a credential on every write instead.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use axum::http::StatusCode;
use pingers_anon::request_digest;
use pingers_crypto::{GroupRequest, Signature, verify_group_request};
use pingers_ohttp::BinaryRequest;
use pingers_proto::{GroupAuthKey, GroupId, RlnAuth, RlnScope, WireDecode as _};
use pingers_quota::{Admission, QuotaError};
use pingers_time::{Clock as _, UnixSeconds};
use rand_core::Rng as _;

use crate::Shared;
use crate::dispatch::Reply;
use crate::wire::{base64url_decode, header, hex_array};

/// The request header carrying an Anon credential.
pub(crate) const AUTHORIZATION: &str = "authorization";
/// The Grp signature header.
pub(crate) const GROUP_SIGNATURE: &str = "pingers-group-signature";
/// The Grp timestamp header.
pub(crate) const GROUP_TIMESTAMP: &str = "pingers-group-time";
/// The group session header, in requests and responses.
pub(crate) const GROUP_SESSION: &str = "pingers-group-session";
/// How far a Grp time may be from the server's clock: half a bucket (design F.1: within
/// ±120 s a device corrects its clock, up to 450 s it uses it unchanged, beyond that it
/// sends no credentialed request).
pub(crate) const GROUP_CLOCK_SKEW: Duration = Duration::from_secs(450);

const RLN_SCHEME: &str = "PingersRln ";
const TOKEN_SCHEME: &str = "PrivateToken token=";

/// An Anon credential as sent.
enum Credential {
    Rln(Box<RlnAuth>),
    Token(Vec<u8>),
}

fn unauthorized() -> Reply {
    Reply::status(StatusCode::UNAUTHORIZED)
}

fn forbidden() -> Reply {
    Reply::status(StatusCode::FORBIDDEN)
}

/// Parses the `Authorization` header: 401 if it is missing or not one of the two schemes.
fn credential(request: &BinaryRequest) -> Result<Credential, Reply> {
    let value = header(request, AUTHORIZATION).ok_or_else(unauthorized)?;
    if let Some(encoded) = value.strip_prefix(RLN_SCHEME) {
        let bytes = base64url_decode(encoded.trim()).ok_or_else(unauthorized)?;
        return RlnAuth::decode(&bytes)
            .map(|auth| Credential::Rln(Box::new(auth)))
            .map_err(|_| unauthorized());
    }
    if let Some(quoted) = value.strip_prefix(TOKEN_SCHEME) {
        let encoded = quoted.trim().trim_matches('"');
        return base64url_decode(encoded)
            .map(Credential::Token)
            .ok_or_else(unauthorized);
    }
    Err(unauthorized())
}

/// How a refused credential is answered: a stale bucket or root means "prove again" (401);
/// everything else is a credential that will never be good (403).
fn refusal(error: &QuotaError) -> Reply {
    match error {
        QuotaError::StaleBucket | QuotaError::Anon(pingers_anon::AnonError::StaleRoot) => {
            unauthorized()
        }
        QuotaError::Anon(_)
        | QuotaError::NullifierReused
        | QuotaError::DoubleSpend
        | QuotaError::DailyLimit => forbidden(),
        QuotaError::TimeOutOfRange
        | QuotaError::RootMismatch
        | QuotaError::CorruptField { .. }
        | QuotaError::Poisoned
        | _ => Reply::status(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

/// Checks the request's Anon credential for `scope`. Blocking (the spent-token set may be an
/// SQLite file): call it on the blocking pool.
///
/// A [`Admission::Replay`] is the same request again (a retransmission) and is served again.
pub(crate) fn admit(
    shared: &Shared,
    request: &BinaryRequest,
    scope: RlnScope,
) -> Result<Admission, Reply> {
    match credential(request)? {
        Credential::Rln(auth) => {
            if auth.scope != scope {
                return Err(forbidden());
            }
            let digest = request_digest(&request.method, &request.path, &request.body)
                .map_err(|_| Reply::status(StatusCode::INTERNAL_SERVER_ERROR))?;
            shared
                .quota
                .nullifiers
                .admit(&shared.quota.membership, &auth, digest)
                .map_err(|error| refusal(&error))
        }
        Credential::Token(token) => shared
            .quota
            .privacy_pass
            .redeem(&token)
            .map(|()| Admission::Fresh)
            .map_err(|error| refusal(&error)),
    }
}

/// How many writes one session stands in for. A credential buys a session, so this is the
/// most group writes one Anon credential can pay for: without it, one proof would buy
/// unlimited writes to a group for a whole bucket. Far above a member's writes in a bucket
/// (a commit, a few chat messages when the live channel is down, a subscription renewal).
pub(crate) const SESSION_WRITES: u32 = 64;

/// A session: which group it is for, until when, and how many writes it has left.
#[derive(Clone, Copy)]
struct Session {
    group: GroupId,
    expires_at: UnixSeconds,
    writes_left: u32,
}

/// The group sessions in memory. Session ids are random and name no account.
#[derive(Default)]
pub(crate) struct Sessions {
    sessions: Mutex<HashMap<[u8; 32], Session>>,
}

impl Sessions {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<[u8; 32], Session>> {
        // The map holds no invariant a panic could break halfway.
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Spends one write of `id` if it is a live session for `group` with writes left.
    fn spend(&self, id: &[u8; 32], group: &GroupId, now: UnixSeconds) -> bool {
        let mut sessions = self.lock();
        let Some(session) = sessions.get_mut(id) else {
            return false;
        };
        if session.group != *group || now >= session.expires_at {
            return false;
        }
        match session.writes_left.checked_sub(1) {
            Some(left) => {
                session.writes_left = left;
                true
            }
            None => false,
        }
    }

    /// Drops expired sessions (the sweep).
    pub(crate) fn expire(&self, now: UnixSeconds) {
        self.lock().retain(|_, session| now < session.expires_at);
    }
}

/// Starts a session for `group` lasting until the end of the current bucket, and returns the
/// response header that carries it.
pub(crate) fn start_session(shared: &Shared, group: GroupId) -> (&'static str, String) {
    let now = shared.clock.now();
    // A bucket end past the end of time cannot happen; such a session would simply be dead.
    let expires_at = now.bucket().end().unwrap_or(now);
    let mut id = [0_u8; 32];
    shared.with_rng(|rng| rng.fill_bytes(&mut id));
    shared.sessions.lock().insert(
        id,
        Session {
            group,
            expires_at,
            writes_left: SESSION_WRITES,
        },
    );
    (GROUP_SESSION, hex::encode(id))
}

/// Checks the Grp signature of `request` against `keys` and returns the position of the key
/// that verifies it: 401 if the headers are missing or malformed or the time is too far off,
/// 403 if no key verifies it.
pub(crate) fn check_signature(
    shared: &Shared,
    request: &BinaryRequest,
    keys: &[GroupAuthKey],
    body: &[u8],
) -> Result<usize, Reply> {
    let signature = header(request, GROUP_SIGNATURE)
        .and_then(hex_array::<64>)
        .ok_or_else(unauthorized)?;
    let timestamp = header(request, GROUP_TIMESTAMP)
        .and_then(|text| text.parse::<u64>().ok())
        .map(UnixSeconds::new)
        .ok_or_else(unauthorized)?;
    let now = shared.clock.now();
    let skew = if timestamp > now {
        timestamp.saturating_duration_since(now)
    } else {
        now.saturating_duration_since(timestamp)
    };
    if skew > GROUP_CLOCK_SKEW {
        return Err(unauthorized());
    }
    let signed = GroupRequest {
        method: &request.method,
        path: &request.path,
        body,
        timestamp,
    };
    let signature = Signature::from_bytes(signature);
    keys.iter()
        .position(|key| verify_group_request(key, &signed, &signature).is_ok())
        .ok_or_else(forbidden)
}

/// What a group request's Anon half came to.
pub(crate) enum GroupAnon {
    /// A live session was presented, or the request needs no Anon credential.
    Session,
    /// A credential was spent; the answer should carry this new session.
    NewSession((&'static str, String)),
}

/// The Anon half of a Grp request: a live session for `group`, or a credential of scope
/// Group (which starts a session). Blocking, as [`admit`].
pub(crate) fn admit_group(
    shared: &Shared,
    request: &BinaryRequest,
    group: GroupId,
) -> Result<GroupAnon, Reply> {
    let session = header(request, GROUP_SESSION).and_then(hex_array::<32>);
    if let Some(id) = session
        && shared.sessions.spend(&id, &group, shared.clock.now())
    {
        return Ok(GroupAnon::Session);
    }
    let _admission = admit(shared, request, RlnScope::Group)?;
    Ok(GroupAnon::NewSession(start_session(shared, group)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_are_for_one_group_until_their_expiry() {
        let sessions = Sessions::default();
        let group = GroupId::from_bytes([1; 32]);
        let other = GroupId::from_bytes([2; 32]);
        let expires_at = UnixSeconds::new(1_000);
        sessions.lock().insert(
            [7; 32],
            Session {
                group,
                expires_at,
                writes_left: SESSION_WRITES,
            },
        );
        let before = UnixSeconds::new(999);
        assert!(sessions.spend(&[7; 32], &group, before));
        assert!(!sessions.spend(&[7; 32], &other, before));
        assert!(!sessions.spend(&[8; 32], &group, before));
        assert!(!sessions.spend(&[7; 32], &group, expires_at));
        sessions.expire(before);
        assert_eq!(sessions.lock().len(), 1);
        sessions.expire(expires_at);
        assert!(sessions.lock().is_empty());
    }

    #[test]
    fn a_session_stands_in_for_a_bounded_number_of_writes() {
        let sessions = Sessions::default();
        let group = GroupId::from_bytes([1; 32]);
        sessions.lock().insert(
            [7; 32],
            Session {
                group,
                expires_at: UnixSeconds::new(1_000),
                writes_left: SESSION_WRITES,
            },
        );
        let now = UnixSeconds::new(1);
        for _ in 0..SESSION_WRITES {
            assert!(sessions.spend(&[7; 32], &group, now));
        }
        assert!(!sessions.spend(&[7; 32], &group, now));
    }
}
