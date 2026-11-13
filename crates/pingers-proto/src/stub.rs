// SPDX-License-Identifier: AGPL-3.0-only
//! Placeholder bodies for contract functions that are not implemented yet.

use crate::ProtoError;

/// Consumes a stub's inputs and reports [`ProtoError::NotImplemented`].
///
/// Taking the inputs by value keeps the compiler quiet about unused parameters without
/// renaming them, so the public signature stays exactly as the contract states. Delete each
/// call as its function is implemented; delete this module when no calls remain.
pub(crate) fn not_implemented<Inputs, Output>(inputs: Inputs) -> Result<Output, ProtoError> {
    drop(inputs);
    Err(ProtoError::NotImplemented)
}
