// SPDX-License-Identifier: AGPL-3.0-only
//! Typed repositories, one per table (or per small family of tables that change together).

pub(crate) mod account;
pub(crate) mod anon;
pub(crate) mod board;
pub(crate) mod mailbox;
pub(crate) mod meetup;
pub(crate) mod mls;
pub(crate) mod party;
pub(crate) mod party_state;
pub(crate) mod prover;
pub(crate) mod relay;
pub(crate) mod spot;
pub(crate) mod sync;

use crate::StoreError;

/// Turns "no row changed" into [`StoreError::NotFound`] for updates and deletes of one row.
pub(crate) fn expect_one(changed: usize, table: &'static str) -> Result<(), StoreError> {
    if changed == 0 {
        Err(StoreError::NotFound { table })
    } else {
        Ok(())
    }
}
