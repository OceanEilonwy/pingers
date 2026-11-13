// SPDX-License-Identifier: AGPL-3.0-only
//! SQLCipher storage.
//!
//! Encrypted local storage for the client: SQLCipher through `rusqlite` with
//! `bundled-sqlcipher`; versioned migrations; a typed repository per table; the pingers-mls
//! storage trait; a cross-process lock for the app and its background services; crash-safe
//! transactions. No location history table, by construction.
//!
//! # Use
//!
//! ```no_run
//! # fn run(path: &std::path::Path, key_bytes: zeroize::Zeroizing<[u8; 32]>)
//! #     -> Result<(), pingers_store::StoreError> {
//! use pingers_store::{Database, DatabaseKey};
//!
//! // The shell unwraps the key from the Android Keystore and passes the bytes in.
//! let database = Database::open(path, DatabaseKey::from_bytes(key_bytes))?;
//! // One session per core entry point (a wake, a push, a user action).
//! let session = database.session()?;
//! session.transaction(|session| {
//!     let parties = session.parties().list()?;
//!     // ... MLS operations through `MlsContext::new(session, ..)` join this transaction ...
//!     # let _ = parties;
//!     Ok::<_, pingers_store::StoreError>(())
//! })?;
//! # Ok(())
//! # }
//! ```
//!
//! # Guarantees
//!
//! - **Encrypted at rest** with the caller's 32-byte key, used raw (no KDF). A wrong key fails
//!   with [`StoreError::WrongKey`]. The key stays in zeroed-on-drop memory; the store never
//!   touches a platform key store (design A.4: the shell wraps it in the Keystore).
//! - **One writer at a time across processes.** A [`Session`] holds an exclusive `flock` on
//!   `<database>-lock` for its lifetime (design B.7). The lock dies with the process.
//! - **Crash-safe.** WAL journal, `synchronous = FULL`; [`Session::transaction`] commits all
//!   or nothing, including on a panic.
//! - **Deleted means gone.** `secure_delete` zeroes freed pages; ending a Meetup deletes every
//!   row that held its keys and content, its MLS group state included, then truncates the WAL
//!   ([`Meetups::end`]).
//! - **No location history.** Coordinates exist only in Spot and zone geometry and in the
//!   latest decrypted position of a granted Meetup peer (one row per peer, deleted with the
//!   Meetup). A test walks the schema to keep it that way.
//!
//! # Decisions
//!
//! - **Sessions, not a shared connection.** Each [`Session`] opens its own connection under
//!   the lock. With a raw key, opening costs no KDF, and it makes the lock the only
//!   synchronisation: two threads in one process serialise exactly as two processes do.
//! - **`MlsStorage` is implemented on [`Session`]**, so MLS writes join the session's
//!   transaction. The trait's error kinds have no source, so database errors become
//!   `StorageError::Failed` (busy or locked: `StorageError::Unavailable`) and undecodable
//!   values `StorageError::Corrupt`.
//! - **[`Meetups::end`] deletes the group's MLS rows too**, under pingers-mls's
//!   `group_storage_prefix`, so no caller can end a Meetup and leave its epoch secrets behind
//!   (style guide §8, design B.5). Calling `delete_group` as well is harmless.
//! - **`lease.records` is normalised** into `lease_slot` (four rows), and the store keeps only
//!   the current lease: replacing it deletes the old one, so past buckets' Spots are not kept.
//! - **`meetup_self_lk.gk_sk` moved to `meetup.gk_sk`.** The grant key pair is per Meetup, not
//!   per location-key generation; repeating the secret in every generation's row added
//!   copies of a secret for nothing.
//! - **`location_grant.expires_at` is always set**, because pingers-crypto's grant always
//!   carries an expiry (the Meetup's end or 15 minutes); `scope` records which.
//! - **`meetup_peer.last_location` is six `location_*` columns** with CHECKs: all present or
//!   all absent, and only while the peer's location key is held (design: "NULL unless their
//!   LK is held").
//! - **`settings.location_visibility` per-Party override** is a nullable column on `party`.
//! - **`rln_tree`** is two tables, `rln_tree_leaf` (the leaf log) and `rln_tree_node`
//!   (cached interior nodes).
//! - **Store-defined types** for values no lower crate defines: user-entered names
//!   ([`PartyName`], [`SpotName`], ...), [`ZoneId`], [`Avatar`], [`UnspentToken`],
//!   [`TokenKeyId`], [`TreePosition`], [`TreeLevel`], [`TreeNodeHash`], [`RootEpoch`].
//!   Privacy Pass tokens are opaque bytes so this crate does not link pingers-anon's prover.
//! - **`server_pin` is a [`pingers_proto::KeyConfigHash`]**: the pinned hash of the server's
//!   OHTTP key configuration.
//! - **`spot.created_by` is the creator's account key**, not a leaf index, because leaves are
//!   reused after removals.
//! - **Unsigned values past `i64::MAX`** are refused ([`StoreError::OutOfRange`]), never
//!   wrapped.
//!
//! # Mutation testing
//!
//! `cargo mutants -p pingers-store` leaves six survivors, all waiting on stubs in lower
//! crates rather than on missing tests:
//!
//! - Polygon geometry (`decode_fence`'s polygon arm and `Reader::u16`, four mutants):
//!   `pingers_geo::Polygon::new` still returns `NotImplemented`, so no polygon can be built
//!   to round-trip. Add a polygon round-trip test when pingers-geo lands it.
//! - `RlnIdentities::save` and `load` (two mutants): `pingers_anon::RlnSecret` cannot be
//!   constructed yet. The column round-trip beneath them (`save_row`, `load_row`) is tested.
#![forbid(unsafe_code)]

mod codec;
mod database;
mod error;
mod key;
mod lock;
mod migrations;
#[cfg(test)]
mod privacy_tests;
mod repo;
#[cfg(test)]
mod test_support;
mod types;

pub use database::{Database, Session};
pub use error::StoreError;
pub use key::DatabaseKey;
pub use repo::anon::{
    HeldToken, PrivacyPassTokens, RlnCounters, RlnIdentities, RlnIdentity, RlnTree, TreeLeaf,
};
pub use repo::board::{
    BoardSeen, KnockState, KnocksOut, LeaseSlot, Leases, OutgoingKnock, SEEN_SEGMENTS_KEPT,
    StoredLease,
};
pub use repo::mailbox::{
    Mailbox, MailboxPurpose, MailboxState, Mailboxes, StoredSubscription, Subscriptions,
};
pub use repo::meetup::{
    GrantScope, GrantedKey, LocationAsks, LocationGrants, LocationRequest, LocationRequestState,
    Meetup, MeetupMessages, MeetupPeer, MeetupPeers, Meetups, OutgoingGrant, RequestDirection,
    SelfLocationKey, SelfLocationKeys, StoredChat,
};
pub use repo::party::{
    AlwaysAllow, Identities, Identity, LocationVisibility, Member, Members, Parties, Party,
    Verification,
};
pub use repo::relay::{
    ActiveFallback, FallbackKind, Fallbacks, PushRelay, PushRelays, RelaySource, Settings,
    WakePushes,
};
pub use repo::spot::{
    ExclusionZone, ExclusionZones, Presence, Spot, SpotPresenceRecord, SpotStatus, Spots,
};
pub use types::{
    Avatar, FallbackReason, MemberName, OperatorName, PartyName, RootEpoch, SpotName, TokenKeyId,
    TreeLevel, TreeNodeHash, TreePosition, UnspentToken, ZoneId, ZoneName,
};
