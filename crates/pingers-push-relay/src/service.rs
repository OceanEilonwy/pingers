// SPDX-License-Identifier: AGPL-3.0-only
//! The relay's logic, without HTTP: authenticate the server, rate-limit, open the blob under
//! its bind, check expiry, rate-limit the token, send one content-free push (design B.4.9, D).

use std::sync::{Mutex, PoisonError};

use pingers_crypto::{KeyedHashKey, KeyedHashPurpose, keyed_hash};
use pingers_proto::{
    PushPlatform, PushRelayConfig, PushRelayKeyId, PushRequest, TimeToLive, WireDecode as _,
};
use pingers_time::{Clock, UnixSeconds};

use crate::counts::Counts;
use crate::credentials::{ServerIndex, ServerRegistry};
use crate::delivery::{DeliveryOutcome, MAX_PUSH_TTL, PushMessage, PushSender, WakeClass};
use crate::keys::{KeyRing, RelayKey};
use crate::opener::{OpenError, TokenOpener};
use crate::rate::{Decision, Limiter, RateLimit};

/// How many partly drained per-token buckets are kept before new tokens are refused. At the
/// default 30 pushes per minute a bucket refills within two minutes, so this is far above any
/// honest load.
const MAX_TRACKED_TOKENS: usize = 1_000_000;

/// How far ahead of the relay's clock a device's clock may be when it seals `not_after`. A blob
/// whose `not_after` is further than `max_ttl` plus this from now was not sealed under the
/// published `max_ttl` (or comes from a badly wrong clock) and is refused.
pub const CLOCK_SKEW_ALLOWANCE: core::time::Duration = core::time::Duration::from_hours(1);

/// What became of one `POST /v1/push`; the HTTP layer maps each to a status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PushOutcome {
    /// Sent to the push service. 202.
    Accepted,
    /// The blob will never work: its key is retired or unknown, it has expired, it lives
    /// longer than `max_ttl` allows, its plaintext is malformed, or the push service rejected
    /// the token. 410; the server deletes it.
    TokenGone,
    /// The blob does not open under this bind. 422.
    DoesNotOpen,
    /// The server or the token is over its rate limit. 429.
    RateLimited,
    /// Missing or unknown server credential. 401.
    Unauthorized,
    /// The body is not a push request. 400.
    BadRequest,
    /// The token is for a platform this relay does not serve. 501; the blob is kept.
    PlatformUnsupported,
    /// The push service failed. 502.
    UpstreamFailed,
    /// The push service did not answer in time. 504.
    UpstreamTimedOut,
    /// The relay itself failed. 500.
    Internal,
}

/// The per-token rate-limit key: in place of the design's `SHA-256(push_token)` (C.1a), a keyed
/// hash, so a memory dump does not confirm a guessed token without the key. It never leaves
/// memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct TokenFingerprint([u8; 32]);

/// What `GET /status` reports, for an operator's status panel: who runs the relay, what it
/// delivers, and where its key rotation stands. No secrets, and no counts that would show how
/// much it is used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayStatus {
    /// Who runs the relay.
    pub operator: String,
    /// The platforms it delivers on.
    pub platforms: Vec<PushPlatform>,
    /// The key devices seal to now; `None` if no key is active.
    pub published_key: Option<PushRelayKeyId>,
    /// The key announced to take over next, if one is scheduled.
    pub next_key: Option<PushRelayKeyId>,
    /// When that key activates.
    pub next_key_activates_at: Option<UnixSeconds>,
}

/// Everything [`Relay::new`] needs.
pub struct RelayParts<O, S, C> {
    /// Opens sealed tokens.
    pub opener: O,
    /// Sends pushes.
    pub sender: S,
    /// Wall clock, for expiry, key rotation and rate limits.
    pub clock: C,
    /// The relay's keys.
    pub keys: KeyRing,
    /// Servers allowed to use the relay.
    pub servers: ServerRegistry,
    /// Keys the per-token rate-limit fingerprints.
    pub token_hash_key: KeyedHashKey,
    /// Per-token limit.
    pub token_limit: RateLimit,
    /// Published in the key config: the longest lifetime a device should seal.
    pub max_ttl: TimeToLive,
    /// Published in the key config: who runs this relay.
    pub operator: String,
}

/// The push relay.
pub struct Relay<O, S, C> {
    opener: O,
    sender: S,
    clock: C,
    keys: KeyRing,
    servers: ServerRegistry,
    token_limit: RateLimit,
    max_ttl: TimeToLive,
    operator: String,
    server_limiter: Mutex<Limiter<ServerIndex>>,
    token_limiter: Mutex<Limiter<TokenFingerprint>>,
    token_hash_key: KeyedHashKey,
    counts: Counts,
}

impl<O: TokenOpener, S: PushSender, C: Clock> Relay<O, S, C> {
    /// A relay from its parts.
    #[must_use]
    pub fn new(parts: RelayParts<O, S, C>) -> Self {
        Self {
            opener: parts.opener,
            sender: parts.sender,
            clock: parts.clock,
            keys: parts.keys,
            servers: parts.servers,
            token_limit: parts.token_limit,
            max_ttl: parts.max_ttl,
            operator: parts.operator,
            // One bucket per configured server is all this map ever holds.
            server_limiter: Mutex::new(Limiter::new(usize::MAX)),
            token_limiter: Mutex::new(Limiter::new(MAX_TRACKED_TOKENS)),
            token_hash_key: parts.token_hash_key,
            counts: Counts::default(),
        }
    }

    /// The outcome counters.
    pub const fn counts(&self) -> &Counts {
        &self.counts
    }

    /// The `GET /v1/config` body to publish now, or `None` if no key is active.
    pub fn published_config(&self) -> Option<PushRelayConfig> {
        Some(self.config_for(self.keys.published(self.clock.now())?))
    }

    /// The `GET /v1/config/next` body: the config devices will fetch once the next key
    /// activates, or `None` if no key is scheduled. Announcing it early lets members add its
    /// hash to their pins before the relay starts publishing it (two-slot rotation).
    pub fn next_config(&self) -> Option<PushRelayConfig> {
        Some(self.config_for(self.keys.next(self.clock.now())?))
    }

    /// The `GET /status` facts: nothing secret and no counts.
    pub fn status(&self) -> RelayStatus {
        let now = self.clock.now();
        let next = self.keys.next(now);
        RelayStatus {
            operator: self.operator.clone(),
            platforms: self.platforms(),
            published_key: self.keys.published(now).map(RelayKey::key_id),
            next_key: next.map(RelayKey::key_id),
            next_key_activates_at: next.map(RelayKey::activated_at),
        }
    }

    fn platforms(&self) -> Vec<PushPlatform> {
        [PushPlatform::UnifiedPush, PushPlatform::Fcm]
            .into_iter()
            .filter(|platform| self.sender.supports(*platform))
            .collect()
    }

    fn config_for(&self, key: &RelayKey) -> PushRelayConfig {
        PushRelayConfig {
            key_config: key.key_config(),
            platforms: self.platforms(),
            max_time_to_live: self.max_ttl,
            operator: self.operator.clone(),
        }
    }

    /// Handles one push request from a server presenting `bearer`, and counts the outcome.
    pub async fn push(&self, bearer: Option<&[u8]>, body: &[u8]) -> PushOutcome {
        let outcome = self.push_uncounted(bearer, body).await;
        self.counts.record(outcome);
        outcome
    }

    async fn push_uncounted(&self, bearer: Option<&[u8]>, body: &[u8]) -> PushOutcome {
        let Some(bearer) = bearer else {
            return PushOutcome::Unauthorized;
        };
        let server = match self.servers.authenticate(bearer) {
            Ok(Some(server)) => server,
            Ok(None) => return PushOutcome::Unauthorized,
            Err(_) => return PushOutcome::Internal,
        };
        let Some(server_limit) = self.servers.limit(server) else {
            return PushOutcome::Internal;
        };
        let now = self.clock.now();
        let server_decision = self
            .server_limiter
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .try_acquire(server, server_limit, now);
        if server_decision == Decision::Limited {
            return PushOutcome::RateLimited;
        }
        let Ok(request) = PushRequest::decode(body) else {
            return PushOutcome::BadRequest;
        };
        self.open_and_send(&request).await
    }

    async fn open_and_send(&self, request: &PushRequest) -> PushOutcome {
        let now = self.clock.now();
        let Some(key) = self.keys.for_opening(request.key_id, now) else {
            // Retired (or never ours): the blob can never open here again.
            return PushOutcome::TokenGone;
        };
        let plaintext = match self.opener.open(key, &request.bind, &request.blob) {
            Ok(plaintext) => plaintext,
            Err(OpenError::DoesNotOpen) => return PushOutcome::DoesNotOpen,
            Err(OpenError::Malformed) => return PushOutcome::TokenGone,
            Err(OpenError::Unavailable) => return PushOutcome::Internal,
        };
        if now > plaintext.not_after {
            return PushOutcome::TokenGone;
        }
        // Design B.4.9: a blob lives at most max_ttl. Without this a far-future not_after
        // could be replayed until the key retires.
        let latest = now
            .checked_add(self.max_ttl.duration())
            .and_then(|limit| limit.checked_add(CLOCK_SKEW_ALLOWANCE));
        if latest.is_none_or(|latest| plaintext.not_after > latest) {
            return PushOutcome::TokenGone;
        }
        if !self.sender.supports(plaintext.platform) {
            return PushOutcome::PlatformUnsupported;
        }
        let Ok(fingerprint) = keyed_hash(
            &self.token_hash_key,
            KeyedHashPurpose::PushTokenRateLimit,
            plaintext.token.as_bytes(),
        ) else {
            return PushOutcome::Internal;
        };
        let fingerprint = TokenFingerprint(*fingerprint.as_bytes());
        let token_decision = self
            .token_limiter
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .try_acquire(fingerprint, self.token_limit, now);
        if token_decision == Decision::Limited {
            return PushOutcome::RateLimited;
        }
        let class = WakeClass::of(plaintext.class, request.urgency);
        let message = PushMessage {
            handle: request.handle,
            priority: class.priority(),
            time_to_live: plaintext
                .not_after
                .saturating_duration_since(now)
                .min(MAX_PUSH_TTL),
        };
        match self
            .sender
            .send(plaintext.platform, &plaintext.token, &message)
            .await
        {
            DeliveryOutcome::Delivered => PushOutcome::Accepted,
            DeliveryOutcome::TokenGone => PushOutcome::TokenGone,
            DeliveryOutcome::Failed => PushOutcome::UpstreamFailed,
            DeliveryOutcome::TimedOut => PushOutcome::UpstreamTimedOut,
        }
    }
}

#[cfg(test)]
mod tests;
