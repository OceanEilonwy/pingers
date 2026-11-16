// SPDX-License-Identifier: AGPL-3.0-only
//! Server sync: group logs, mailboxes, request queue.
//!
//! Talking to the server, reliably, for everyone else: fetch and apply group log messages;
//! post commits (one per epoch, retry on conflict); mailbox registration and reads;
//! subscriptions with sealed push; the queue for anonymous requests that could not be sent
//! in this wake, with drop rules; token supply (pre-fetched Privacy Pass, RLN proofs made only
//! in the main app). Knows nothing about what the messages mean; it hands them to their owner.
//!
//! # Shape
//!
//! - [`ApiRequest`] has a typed builder for every route the client uses ([`Route`]); each
//!   route has a fixed privacy class ([`Route::class`]) and credential scope
//!   ([`Route::credential`]).
//! - [`SyncEngine::send`] attaches the credential ([`choose_credential`]: an RLN proof in the
//!   main app, otherwise a Privacy Pass token), sends through the router inside the
//!   [`WakeBudget`], and applies the request's [`QueuePolicy`] when it could not be sent.
//!   Anonymous and stream requests are never sent direct: with no relay they are queued or
//!   dropped, and [`SyncEngine::flush_queue`] sends the queue on a later wake.
//! - Typed operations on top: mailboxes ([`SyncEngine::register_mailbox`] and friends),
//!   group logs ([`SyncEngine::pull_group`], [`SyncEngine::commit`] through the
//!   [`GroupLogOwner`] and [`CommitOwner`] traits), the board ([`SyncEngine::board_sync`]),
//!   the membership tree and the token stock ([`SyncEngine::refill_tokens`]).
//!
//! # Wire conventions added here
//!
//! pingers-server's `wire.rs` fixes paths, queries and the read-key header. Two encodings
//! were still open and are chosen here (the server must match): a group request (auth mode
//! Grp) carries `pingers-group-time: <decimal unix seconds>` and
//! `pingers-group-signature: <hex Ed25519 signature>` over pingers-crypto's
//! `GroupRequest { method, path with query, body, timestamp }`; an RLN proof travels as
//! `authorization: PingersRln <base64url, padded, of the 265-byte RlnAuth>` (design D; padded
//! as pingers-anon pads `PrivateToken`).
#![forbid(unsafe_code)]

mod account;
mod base64url;
mod board;
mod budget;
mod clock;
mod credential;
mod engine;
mod error;
mod group;
mod mailbox;
mod membership;
mod pool;
mod prover;
mod queue;
mod registrations;
mod request;
mod server;
mod tokens;

pub use account::{BEARER_LIFETIME, CodeSource, LeafStanding, Prepared};
pub use board::NewLease;
pub use budget::{MIN_REQUEST_TIME, WakeBudget};
pub use clock::{ClockOffset, ClockStatus, MAX_CORRECTION, MAX_SKEW};
pub use credential::{
    CredentialChoice, CredentialPolicy, ProcessRole, ProverStatus, RlnReadiness, choose_credential,
};
pub use engine::{
    FlushReport, NotSentReason, Outcome, Response, SyncEngine, SyncParts, SyncRng, SyncSettings,
};
pub use error::{GroupError, SyncError};
pub use group::{CommitAttempts, CommitOwner, Committed, GroupCall, GroupLogOwner, Pulled};
pub use mailbox::{Delivered, NewMailbox, PushSeal};
pub use membership::MembershipRefresh;
/// The prover self-test plan `SyncSettings::self_test` takes (pingers-anon's).
pub use pingers_anon::SelfTestPlan;
pub use pool::{LEASE_BUCKETS, PoolRefill, PoolTarget, WakePush};
pub use prover::{RETEST_AFTER, SelfTestStanding, SelfTestStart};
pub use queue::{QueueCapacity, QueuePolicy, Sending};
pub use registrations::{GroupMembership, Maintained, Maintenance, Pushes, RENEW_AHEAD};
pub use request::{ApiRequest, GroupLifetime, GroupSigner, GroupStart, Route};
pub use server::{NextPushRelayKey, ScheduledPushRelay, VerifiedPushRelay};
pub use tokens::{DEFAULT_DAILY_LIMIT, Refill, TokenIssuer, TokenStock};
