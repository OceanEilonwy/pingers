// SPDX-License-Identifier: AGPL-3.0-only
//! Placeholder bodies for contract functions that are not implemented yet.

use crate::PresenceError;

/// Consumes a stub's inputs and reports [`PresenceError::NotImplemented`].
///
/// Taking the inputs by value keeps the public signature exactly as the contract states.
/// Delete each call as its function is implemented; delete this module when none remain.
pub(crate) fn not_implemented<Inputs, Output>(inputs: Inputs) -> Result<Output, PresenceError> {
    drop(inputs);
    Err(PresenceError::NotImplemented)
}
