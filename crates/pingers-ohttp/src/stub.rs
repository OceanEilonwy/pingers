// SPDX-License-Identifier: AGPL-3.0-only
//! Placeholder bodies for contract functions that are not implemented yet.

use crate::OhttpError;

/// Consumes a stub's inputs and reports [`OhttpError::NotImplemented`].
///
/// Taking the inputs by value keeps the public signature exactly as the contract states.
/// Delete each call as its function is implemented; delete this module when none remain.
pub(crate) fn not_implemented<Inputs, Output>(inputs: Inputs) -> Result<Output, OhttpError> {
    drop(inputs);
    Err(OhttpError::NotImplemented)
}

/// An uninhabited marker held by types whose constructor is still a stub, so their infallible
/// methods can be written as `match self.unimplemented {}`. Replace it with real state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unimplemented {}
