// SPDX-License-Identifier: AGPL-3.0-only
//! Server: board records, expiry, paging, segment hashes.
//!
//! The blind board of design B.4.3, held in memory only (design C.1):
//!
//! - [`Board::sync`] appends a lease, revokes the lease it replaces, and returns the first page
//!   taken after the append;
//! - [`Board::page`] serves the following pages from a sealed [`pingers_proto::BoardCursor`],
//!   valid [`CURSOR_LIFETIME`];
//! - [`Board::revoke`] removes a lease's records;
//! - [`Board::expire`] drops a segment when bucket `b0 + 4` starts;
//! - each page carries SHA-256 of every closed segment it covers, for the equivocation check.
//!
//! # Decisions
//!
//! - **No storage trait and no `sqlite` feature.** The architecture gives every server crate a
//!   storage trait, but design C.1 says the board is never written to disk; a trait with one
//!   in-memory implementation would only add indirection. Restarting the server empties the
//!   board, and devices re-post within one cadence.
//! - **Cursors are sealed, not only authenticated.** pingers-crypto has no general MAC, so
//!   the cursor is sealed with its snapshot AEAD under a key used for nothing else (see
//!   [`CursorKey`]).
//!   Authentication is what the design needs; the encryption also hides segment positions.
//! - **A segment's hash covers every record appended to it**, revoked or not, in arrival
//!   order. It is fixed once the segment is closed (two buckets after its start, when no lease
//!   can start there any more), so revocations later in the lease cannot make two honest
//!   readers see different hashes.
//! - **Rate limits are not here.** pingers-quota checks the membership proof before a sync
//!   reaches the board.
#![forbid(unsafe_code)]

mod board;
mod cursor;
mod error;

pub use board::{Board, PageSize};
pub use cursor::{CURSOR_LIFETIME, CursorKey};
pub use error::BoardError;
