// SPDX-License-Identifier: AGPL-3.0-only
//! The crate's error type. Variants name what failed, never identifiers, tokens or bodies
//! (style guide §9).

use crate::Route;

/// Why a sync operation failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SyncError {
    /// No OHTTP relay was reachable for a request that must not be queued (an identified
    /// request never sees this; anonymous and stream requests are never sent direct).
    #[error("no OHTTP relay reachable for {route:?}")]
    NoRelay {
        /// The route that could not be sent.
        route: Route,
    },
    /// The gateway's key config differs from the pinned hash. Raise
    /// `Notice::RelayKeyMismatch`; anonymous traffic stays stopped until the transport is
    /// repinned.
    #[error("the OHTTP gateway key does not match the pin")]
    RelayKeyMismatch,
    /// A push relay's key config is not one of its pinned hashes: seal no push token to
    /// it, and raise `Notice::RelayKeyMismatch` (design B.4.9).
    #[error("the push relay key does not match its pins")]
    PushRelayKeyMismatch,
    /// The device clock is more than half a bucket from the server's (design F.1): nothing is
    /// posted. Raise `Notice::ClockSkew`.
    #[error("device clock is skewed from the server's")]
    ClockSkew {
        /// Server minus device.
        offset: crate::ClockOffset,
    },
    /// The wake budget ran out before or while the request was made. If it ran out while the
    /// request was in flight, the server may have received it.
    #[error("wake budget exhausted before {route:?} completed")]
    BudgetExhausted {
        /// The route that was not completed.
        route: Route,
    },
    /// The server answered with a status this operation does not expect.
    #[error("{route:?} answered HTTP {status}")]
    Status {
        /// The route.
        route: Route,
        /// The HTTP status.
        status: u16,
    },
    /// The server's answer did not decode.
    #[error("{route:?} answered with a body that does not decode")]
    BadResponse {
        /// The route.
        route: Route,
        /// Why.
        #[source]
        source: pingers_proto::ProtoError,
    },
    /// The request could not be encoded.
    #[error("cannot encode the request")]
    Encode(#[source] pingers_proto::ProtoError),
    /// No anonymous credential is available: the server accepts none this process may make,
    /// or the Privacy Pass stock is empty and proofs are not possible here.
    #[error("no anonymous credential available")]
    NoCredential,
    /// Making an RLN proof or a Privacy Pass request failed.
    #[error("anonymous credential failed")]
    Anon(#[from] pingers_anon::AnonError),
    /// Sealing a push token or signing a group request failed.
    #[error("cryptography failed")]
    Crypto(#[from] pingers_crypto::CryptoError),
    /// The local store failed.
    #[error("local store failed")]
    Store(#[from] pingers_store::StoreError),
    /// The transport failed in a way not covered above (timeout, connection, status).
    #[error("transport failed")]
    Transport(#[source] pingers_transport::TransportError),
    /// A mailbox this device does not hold was named.
    #[error("unknown mailbox")]
    UnknownMailbox,
    /// A counter or time computation left its range.
    #[error("{0} out of range")]
    OutOfRange(&'static str),
    /// A request could not be encoded for the store's queue, or a queued one read back from
    /// it does not decode (the entry is dropped).
    #[error("a queued request does not encode or decode")]
    QueuedRequest,
    /// A blocking task (SQLCipher or the prover) did not finish.
    #[error("background task failed")]
    Internal,
}

/// Why a group log operation failed: the sync side or the owner applying the log.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GroupError<E: std::error::Error + 'static> {
    /// Talking to the server failed.
    #[error(transparent)]
    Sync(#[from] SyncError),
    /// The owner could not apply an entry or prepare a commit.
    #[error("the group's owner failed")]
    Owner(#[source] E),
    /// Every attempt to commit lost to another member's commit for the same epoch.
    #[error("commit lost {attempts} times to other commits")]
    ConflictsExhausted {
        /// How many attempts were made.
        attempts: u32,
    },
}
