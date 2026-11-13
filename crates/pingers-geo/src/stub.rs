// SPDX-License-Identifier: AGPL-3.0-only
//! Placeholders for contract items that are not implemented yet.

use crate::GeoError;

/// Consumes a stub's inputs and reports [`GeoError::NotImplemented`].
///
/// Taking the inputs by value keeps the public signature exactly as the contract states.
/// Delete each call as its function is implemented; delete this module when none remain.
pub(crate) fn not_implemented<Inputs, Output>(inputs: Inputs) -> Result<Output, GeoError> {
    drop(inputs);
    Err(GeoError::NotImplemented)
}

/// An uninhabited marker held by types whose constructor is still a stub.
///
/// Because no value of it exists, a type holding one cannot be built, so its infallible
/// methods can be written as `match self.unimplemented {}` without lying about a result.
/// Replace the field with real state when implementing the type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unimplemented {}
