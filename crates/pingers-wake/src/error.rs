// SPDX-License-Identifier: AGPL-3.0-only
//! The crate's error types.

/// Why a port (pingers-sync, pingers-parties, pingers-meetup-session, the platform) could not do
/// what the wake asked. pingers-client maps each crate's own error onto these.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PortError {
    /// The server, relay or network could not be reached.
    #[error("unreachable")]
    Unreachable,
    /// The other side refused the request.
    #[error("refused")]
    Refused,
    /// Local state needed for the request is missing or unusable.
    #[error("local state unavailable")]
    Unavailable,
}

/// Why a wake failed. Network failures and an exhausted budget are not errors: they end the
/// wake early and are reported in [`crate::WakeEnding`]. Never carries identifiers, keys or
/// coordinates.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WakeError {
    /// A protocol step failed in a way the inputs should have ruled out.
    #[error("presence protocol step failed")]
    Presence(#[from] pingers_presence::PresenceError),
}
